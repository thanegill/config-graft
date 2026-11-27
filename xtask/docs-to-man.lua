-- Turns a docs/*.md guide into a section 7 man page. Metadata from the caller:
-- `name` (the page name), `description` (the NAME line), `guides` (the stems
-- converted alongside it) and `see_also` (page references).

local REPO = "https://github.com/thanegill/config-graft/blob/master/"

local function as_list(value)
  if value == nil then
    return {}
  end
  if pandoc.utils.type(value) ~= "List" then
    return { value }
  end
  return value
end

local guides = {}

local function page_reference(page)
  local name, section = page:match("^(.-)(%(%d%))$")
  return { pandoc.Strong(pandoc.Str(name)), pandoc.Str(section) }
end

local function with_url(content, url)
  if pandoc.utils.stringify(content) == url then
    return { pandoc.Str(url) }
  end
  local out = pandoc.List(content)
  out:insert(pandoc.Str(" <" .. url .. ">"))
  return out
end

local function Meta(meta)
  for _, stem in ipairs(as_list(meta.guides)) do
    guides[pandoc.utils.stringify(stem)] = true
  end
end

local function Link(link)
  local target = link.target
  local stem = target:gsub("#.*$", ""):match("^([%w-]+)%.md$")
  if stem then
    if not guides[stem] then
      error("link to " .. target .. ", which is not converted to a man page")
    end
    return page_reference("config-graft-" .. stem .. "(7)")
  end
  if target:match("^%a[%w+.-]*:") then
    return with_url(link.content, target)
  end
  if target:match("^%.%./") then
    return with_url(link.content, REPO .. target:sub(4))
  end
  return link.content
end

local function upper_case(inlines)
  return inlines:walk({
    Str = function(str)
      str.text = pandoc.text.upper(str.text)
      return str
    end,
  })
end

local function Pandoc(doc)
  local meta = doc.meta
  local blocks = pandoc.List()
  for _, block in ipairs(doc.blocks) do
    if block.t == "Header" and block.level == 1 then
      blocks:insert(pandoc.Header(1, "NAME"))
      blocks:insert(pandoc.Plain(pandoc.Inlines(
        pandoc.utils.stringify(meta.name) .. " - " .. pandoc.utils.stringify(meta.description))))
      blocks:insert(pandoc.Header(1, "DESCRIPTION"))
      blocks:insert(pandoc.RawBlock("man", ".nr HY 0\n.nh\n.ds AD l\n.ad l"))
    elseif block.t == "Header" then
      block.level = block.level - 1
      if block.level == 1 then
        block.content = upper_case(block.content)
      end
      blocks:insert(block)
    else
      blocks:insert(block)
    end
  end
  local refs = pandoc.List()
  for i, item in ipairs(as_list(meta.see_also)) do
    if i > 1 then
      refs:insert(pandoc.Str(","))
      refs:insert(pandoc.Space())
    end
    refs:extend(page_reference(pandoc.utils.stringify(item)))
  end
  if #refs > 0 then
    blocks:insert(pandoc.Header(1, "SEE ALSO"))
    blocks:insert(pandoc.Para(refs))
  end
  doc.blocks = blocks
  return doc
end

return { { Meta = Meta }, { Link = Link }, { Pandoc = Pandoc } }
