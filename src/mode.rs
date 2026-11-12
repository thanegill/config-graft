//! The mode a byte-format rewrite gives TARGET.
//!
//! A rewrite replaces TARGET with a new file, so nothing of the old mode survives
//! unless it is set again. The permission bits always are. Of setuid, setgid and
//! sticky, the rewrite keeps what an in-place write by this user would keep on this
//! platform. That decision is a pure function here, so every branch is testable
//! without a real chmod or chown.

use std::fmt;
use std::fs::{File, Metadata, Permissions};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

const SETUID: u32 = 0o4000;
const SETGID: u32 = 0o2000;
const STICKY: u32 = 0o1000;
const SPECIAL_BITS: u32 = SETUID | SETGID | STICKY;
const GROUP_EXEC: u32 = 0o010;
/// Everything `chmod` sets: the permission bits plus setuid, setgid and sticky.
const MODE_BITS: u32 = 0o7777;

/// A file's owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owner {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
}

impl Owner {
    pub(crate) fn of(metadata: &Metadata) -> Owner {
        Owner {
            uid: metadata.uid(),
            gid: metadata.gid(),
        }
    }
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.uid, self.gid)
    }
}

/// Who a rewrite writes TARGET as, which decides what an in-place write would keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Writer {
    /// The rewrite leaves the file owned by `to` instead of `from`.
    NewOwner {
        from: Owner,
        to: Owner,
    },
    Privileged,
    Unprivileged,
}

impl Writer {
    /// `written` is the owner of the file the rewrite puts in TARGET's place, and
    /// `created` the owner it was created with, which is this process: a uid of 0 is
    /// privileged even once the file is handed back to TARGET's owner. A new owner
    /// outranks privilege: the rewrite is not an in-place write at all then.
    pub(crate) fn of(previous: Owner, written: Owner, created: Owner) -> Writer {
        if written != previous {
            Writer::NewOwner {
                from: previous,
                to: written,
            }
        } else if created.uid == 0 {
            Writer::Privileged
        } else {
            Writer::Unprivileged
        }
    }
}

/// What an unprivileged in-place write does to setgid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetgidOnWrite {
    /// Cleared only when group-exec is set: Linux's rule, used on every host but macOS.
    ClearedWithGroupExec,
    /// Always cleared: macOS.
    Cleared,
}

pub(crate) const HOST_SETGID_ON_WRITE: SetgidOnWrite = if cfg!(target_os = "macos") {
    SetgidOnWrite::Cleared
} else {
    SetgidOnWrite::ClearedWithGroupExec
};

/// The mode a rewrite gives a file whose mode was `previous` (file-type bits
/// ignored): its permission bits, and of setuid/setgid/sticky what an in-place write
/// by `writer` would keep. A new owner keeps neither setuid nor setgid, a privileged
/// writer keeps every bit, and anyone else loses setuid and, per `setgid_on_write`,
/// setgid. Sticky is never cleared.
pub(crate) fn rewritten_mode(previous: u32, writer: Writer, setgid_on_write: SetgidOnWrite) -> u32 {
    let previous = previous & MODE_BITS;
    let cleared = match (writer, setgid_on_write) {
        (Writer::NewOwner { .. }, _) => SETUID | SETGID,
        (Writer::Privileged, _) => 0,
        (Writer::Unprivileged, SetgidOnWrite::Cleared) => SETUID | SETGID,
        (Writer::Unprivileged, SetgidOnWrite::ClearedWithGroupExec)
            if previous & GROUP_EXEC != 0 =>
        {
            SETUID | SETGID
        }
        (Writer::Unprivileged, SetgidOnWrite::ClearedWithGroupExec) => SETUID,
    };
    previous & !cleared
}

/// Set `mode` on `file`. Some sandboxes refuse setuid and setgid outright (EPERM),
/// where plain chmod only drops setgid, silently, for a group the writer is not in;
/// without them the write still lands, and the drop is reported like any other.
pub(crate) fn set_mode(file: &File, mode: u32) -> io::Result<()> {
    set_mode_with(mode, |mode| {
        file.set_permissions(Permissions::from_mode(mode))
    })
}

fn set_mode_with(mode: u32, mut chmod: impl FnMut(u32) -> io::Result<()>) -> io::Result<()> {
    match chmod(mode) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && mode & (SETUID | SETGID) != 0 => {
            chmod(mode & !(SETUID | SETGID))
        }
        set => set,
    }
}

/// Why the writer's own rule dropped a special bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeCause {
    /// The rewrite leaves the file with another owner.
    NewOwner { from: Owner, to: Owner },
    /// An in-place write by this user would have dropped it too.
    InPlaceWrite,
}

/// The three modes a rewrite deals with: the file's, the one [`rewritten_mode`]
/// asked for, and the one the file got.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Modes {
    pub(crate) previous: u32,
    pub(crate) requested: u32,
    pub(crate) actual: u32,
}

/// A rewrite that left TARGET with fewer special bits than it had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModeChange {
    pub(crate) from: u32,
    pub(crate) to: u32,
    /// The special bits the writer's rule dropped; `cause` says why.
    pub(crate) ruled_out: u32,
    pub(crate) cause: ModeCause,
    /// The special bits the rule kept but the system would not set.
    pub(crate) refused: u32,
}

impl ModeChange {
    /// The change a rewrite by `writer` made, when it dropped a special bit. A
    /// permission-bit difference alone is not one.
    pub(crate) fn between(modes: Modes, writer: Writer) -> Option<ModeChange> {
        let from = modes.previous & MODE_BITS;
        let ruled_out = from & !modes.requested & SPECIAL_BITS;
        let refused = modes.requested & !modes.actual & SPECIAL_BITS;
        if ruled_out | refused == 0 {
            return None;
        }
        let cause = match writer {
            Writer::NewOwner { from, to } => ModeCause::NewOwner { from, to },
            Writer::Privileged | Writer::Unprivileged => ModeCause::InPlaceWrite,
        };
        Some(ModeChange {
            from,
            to: modes.actual & MODE_BITS,
            ruled_out,
            cause,
            refused,
        })
    }
}

/// `bits`' special bits, named: `setuid and setgid`.
pub(crate) fn special_bit_names(bits: u32) -> String {
    [(SETUID, "setuid"), (SETGID, "setgid"), (STICKY, "sticky")]
        .into_iter()
        .filter(|(bit, _)| bits & bit != 0)
        .map(|(_, name)| name)
        .collect::<Vec<_>>()
        .join(" and ")
}

/// The mode bits of `metadata`, without the file type.
pub(crate) fn mode_of(metadata: &Metadata) -> u32 {
    metadata.mode() & MODE_BITS
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: Owner = Owner { uid: 501, gid: 20 };
    const ROOT: Owner = Owner { uid: 0, gid: 0 };
    const BOTH: [SetgidOnWrite; 2] = [SetgidOnWrite::ClearedWithGroupExec, SetgidOnWrite::Cleared];

    #[test]
    fn a_new_owner_outranks_privilege() {
        assert_eq!(
            Writer::of(ALICE, ROOT, ROOT),
            Writer::NewOwner {
                from: ALICE,
                to: ROOT
            }
        );
        let other_group = Owner { gid: 80, ..ALICE };
        assert!(matches!(
            Writer::of(ALICE, other_group, other_group),
            Writer::NewOwner { .. }
        ));
        assert_eq!(Writer::of(ROOT, ROOT, ROOT), Writer::Privileged);
        assert_eq!(Writer::of(ALICE, ALICE, ALICE), Writer::Unprivileged);
    }

    #[test]
    fn root_that_kept_the_owner_is_still_privileged() {
        assert_eq!(Writer::of(ALICE, ALICE, ROOT), Writer::Privileged);
    }

    #[test]
    fn a_new_owner_loses_setuid_and_setgid_but_keeps_sticky() {
        let other = Owner { uid: 502, ..ALICE };
        let writer = Writer::of(ALICE, other, other);
        for rule in BOTH {
            // No group-exec, so an unprivileged in-place rule would keep setgid here.
            assert_eq!(rewritten_mode(0o7745, writer, rule), 0o1745, "{rule:?}");
        }
    }

    #[test]
    fn a_privileged_writer_keeps_every_bit() {
        for rule in BOTH {
            assert_eq!(
                rewritten_mode(0o7745, Writer::Privileged, rule),
                0o7745,
                "{rule:?}"
            );
        }
    }

    #[test]
    fn an_unprivileged_writer_loses_setuid_and_keeps_sticky() {
        for rule in BOTH {
            assert_eq!(
                rewritten_mode(0o5755, Writer::Unprivileged, rule),
                0o1755,
                "{rule:?}"
            );
        }
    }

    #[test]
    fn an_unprivileged_writer_loses_setgid_per_the_platform_rule() {
        let rewrite = |mode, rule| rewritten_mode(mode, Writer::Unprivileged, rule);
        assert_eq!(rewrite(0o2755, SetgidOnWrite::ClearedWithGroupExec), 0o0755);
        assert_eq!(rewrite(0o2745, SetgidOnWrite::ClearedWithGroupExec), 0o2745);
        assert_eq!(rewrite(0o2755, SetgidOnWrite::Cleared), 0o0755);
        assert_eq!(rewrite(0o2745, SetgidOnWrite::Cleared), 0o0745);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_always_clears_setgid_on_an_unprivileged_write() {
        assert_eq!(HOST_SETGID_ON_WRITE, SetgidOnWrite::Cleared);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_clears_setgid_only_with_group_exec() {
        assert_eq!(HOST_SETGID_ON_WRITE, SetgidOnWrite::ClearedWithGroupExec);
    }

    #[test]
    fn file_type_bits_are_ignored() {
        assert_eq!(
            rewritten_mode(0o100_644, Writer::Unprivileged, SetgidOnWrite::Cleared),
            0o644
        );
    }

    fn change(previous: u32, requested: u32, actual: u32, writer: Writer) -> Option<ModeChange> {
        let modes = Modes {
            previous,
            requested,
            actual,
        };
        ModeChange::between(modes, writer)
    }

    #[test]
    fn a_change_splits_what_the_rule_dropped_from_what_the_system_refused() {
        assert_eq!(change(0o100_644, 0o644, 0o644, Writer::Unprivileged), None);
        assert_eq!(
            change(0o104_640, 0o640, 0o640, Writer::Unprivileged),
            Some(ModeChange {
                from: 0o4640,
                to: 0o640,
                ruled_out: SETUID,
                cause: ModeCause::InPlaceWrite,
                refused: 0,
            })
        );
        let other_group = Owner { gid: 80, ..ALICE };
        let new_owner = Writer::of(ALICE, other_group, other_group);
        assert_eq!(
            change(0o5755, 0o1755, 0o755, new_owner),
            Some(ModeChange {
                from: 0o5755,
                to: 0o755,
                ruled_out: SETUID,
                cause: ModeCause::NewOwner {
                    from: ALICE,
                    to: other_group
                },
                refused: STICKY,
            })
        );
        assert_eq!(
            change(0o2740, 0o2740, 0o740, Writer::Unprivileged).map(|c| (c.ruled_out, c.refused)),
            Some((0, SETGID))
        );
    }

    #[test]
    fn a_permission_bit_difference_alone_is_not_a_dropped_special_bit() {
        assert_eq!(change(0o644, 0o644, 0o755, Writer::Unprivileged), None);
    }

    /// The modes `set_mode_with` asked for, against a chmod that answers `refuse` for
    /// any mode holding setuid or setgid.
    fn chmods(mode: u32, refuse: io::ErrorKind) -> (io::Result<()>, Vec<u32>) {
        let mut asked = Vec::new();
        let result = set_mode_with(mode, |mode| {
            asked.push(mode);
            if mode & (SETUID | SETGID) != 0 {
                Err(io::Error::from(refuse))
            } else {
                Ok(())
            }
        });
        (result, asked)
    }

    #[test]
    fn a_refused_special_bit_is_dropped_and_the_mode_set_without_it() {
        let (result, asked) = chmods(0o7640, io::ErrorKind::PermissionDenied);
        assert!(result.is_ok());
        assert_eq!(asked, [0o7640, 0o1640]);
    }

    #[test]
    fn only_a_refused_special_bit_is_retried() {
        let (result, asked) = chmods(0o1640, io::ErrorKind::PermissionDenied);
        assert!(result.is_ok());
        assert_eq!(asked, [0o1640]);

        let (result, asked) = chmods(0o4640, io::ErrorKind::Other);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Other);
        assert_eq!(asked, [0o4640]);

        let mut asked = Vec::new();
        let result = set_mode_with(0o640, |mode| {
            asked.push(mode);
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(asked, [0o640]);
    }

    #[test]
    fn special_bits_are_named() {
        assert_eq!(special_bit_names(SETUID | SETGID), "setuid and setgid");
        assert_eq!(special_bit_names(STICKY), "sticky");
        assert_eq!(special_bit_names(0o4755), "setuid");
    }
}
