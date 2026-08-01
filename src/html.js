import { parse } from "parse5";

const RECOVERABLE_PARSE_ERRORS = new Set(["missing-doctype"]);
const TEMPLATE_MARKERS = /\{\{|\}\}|\$\{|<%|%>|<#|#>|\[%|%\]/;

export function parseHtmlSource(path, source) {
  const errors = [];
  const document = parse(source, {
    sourceCodeLocationInfo: true,
    onParseError(error) {
      if (!RECOVERABLE_PARSE_ERRORS.has(error.code)) errors.push(error);
    },
  });
  if (errors.length > 0) {
    throw new Error(`Failed to parse ${path}: ${errors.map((error) => error.code).join(", ")}`);
  }

  const links = [];
  const bases = [];
  const elements = [];
  const dynamicAttributes = [];
  const scriptTexts = [];

  function visit(node) {
    if (node.tagName === "script") {
      for (const child of node.childNodes ?? []) {
        if (child.nodeName === "#text" && child.value) scriptTexts.push(child.value);
      }
    }
    if (node.tagName) {
      const attributes = new Map(node.attrs.map((attribute) => [attribute.name, attribute.value]));
      const locations = node.sourceCodeLocation?.attrs;
      if (locations) {
        if (
          node.tagName === "link" &&
          stylesheetRel(attributes.get("rel")) &&
          !attributes.has("disabled")
        ) {
          const href = locatedAttribute(source, locations.href, attributes.get("href"));
          const media = locatedAttribute(source, locations.media, attributes.get("media"));
          const tag = node.sourceCodeLocation?.startTag ?? node.sourceCodeLocation;
          if (href && tag) {
            links.push({
              href: href.value,
              media: media?.value ?? "",
              start: href.start,
              end: href.end,
              tagStart: tag.startOffset,
              tagEnd: tag.endOffset,
            });
          }
        }
        if (node.tagName === "base" && bases.length === 0) {
          const href = locatedAttribute(source, locations.href, attributes.get("href"));
          if (href)
            bases.push({
              href: href.value,
              writable: href.writable,
              start: href.start,
              end: href.end,
            });
        }

        let classAttribute = locatedAttribute(source, locations.class, attributes.get("class"));
        const idAttribute = locatedAttribute(source, locations.id, attributes.get("id"));
        // A class attribute that exists but cannot be located as a writable
        // value (e.g. valueless `<main class>`) must not look absent, or the
        // id-only branch would synthesize a duplicate class attribute.
        const unparsedClass =
          !classAttribute && idAttribute && locations.class
            ? { start: locations.class.startOffset, end: locations.class.endOffset }
            : undefined;
        const dynamic =
          unparsedClass ??
          [classAttribute, idAttribute].find(
            (attribute) =>
              attribute && (!attribute.writable || TEMPLATE_MARKERS.test(attribute.value)),
          ) ??
          (classAttribute && !classAttribute.quoted ? classAttribute : undefined);
        if (dynamic) {
          dynamicAttributes.push({ start: dynamic.start, end: dynamic.end });
        } else if (classAttribute || idAttribute) {
          if (!classAttribute && idAttribute) {
            const insertion = classInsertionOffset(source, node.sourceCodeLocation?.startTag);
            if (insertion !== undefined) {
              classAttribute = { value: "", start: insertion, end: insertion, synthetic: true };
            }
          }
          elements.push({ classAttribute, idAttribute });
        }
      }
    }

    for (const child of node.childNodes ?? []) visit(child);
    if (node.content) visit(node.content);
  }

  visit(document);
  return {
    ...toByteOffsets(source, { links, bases, elements, dynamicAttributes }),
    scriptText: scriptTexts.join("\n"),
  };
}

// Map UTF-16 string indices to UTF-8 byte offsets in one source pass, so
// large documents stay linear instead of rescanning the prefix per site.
export function utf8OffsetMap(source, indices) {
  const sorted = [...new Set(indices)].sort((left, right) => left - right);
  const map = new Map();
  let lastIndex = 0;
  let bytes = 0;
  for (const index of sorted) {
    bytes += Buffer.byteLength(source.slice(lastIndex, index));
    lastIndex = index;
    map.set(index, bytes);
  }
  return map;
}

function toByteOffsets(source, parsed) {
  const offsets = utf8OffsetMap(source, [
    ...parsed.links.flatMap((link) => [link.start, link.end, link.tagStart, link.tagEnd]),
    ...parsed.bases.flatMap((base) => [base.start, base.end]),
    ...parsed.elements.flatMap((element) =>
      [element.classAttribute, element.idAttribute]
        .filter(Boolean)
        .flatMap((value) => [value.start, value.end]),
    ),
    ...parsed.dynamicAttributes.flatMap((value) => [value.start, value.end]),
  ]);
  const offset = (index) => offsets.get(index);
  const attribute = (value) =>
    value && { ...value, start: offset(value.start), end: offset(value.end) };
  return {
    links: parsed.links.map((link) => ({
      ...link,
      start: offset(link.start),
      end: offset(link.end),
      tagStart: offset(link.tagStart),
      tagEnd: offset(link.tagEnd),
    })),
    bases: parsed.bases.map(attribute),
    elements: parsed.elements.map((element) => ({
      classAttribute: attribute(element.classAttribute),
      idAttribute: attribute(element.idAttribute),
    })),
    dynamicAttributes: parsed.dynamicAttributes.map(attribute),
  };
}

function classInsertionOffset(source, startTag) {
  if (!startTag) return undefined;
  let offset = startTag.endOffset - 1;
  if (source[offset] !== ">") return undefined;
  while (offset > startTag.startOffset && /\s/.test(source[offset - 1])) offset -= 1;
  if (source[offset - 1] === "/") offset -= 1;
  return offset;
}

function stylesheetRel(value = "") {
  const tokens = value.toLowerCase().split(/\s+/);
  return tokens.includes("stylesheet") && !tokens.includes("alternate");
}

function locatedAttribute(source, location, parsedValue) {
  if (!location || parsedValue === undefined) return undefined;
  const raw = source.slice(location.startOffset, location.endOffset);
  const equals = raw.indexOf("=");
  if (equals < 0) return undefined;
  let start = equals + 1;
  while (/\s/.test(raw[start] ?? "")) start += 1;
  const quote = raw[start];
  let end;
  const quoted = quote === '"' || quote === "'";
  if (quoted) {
    start += 1;
    end = raw.lastIndexOf(quote);
    if (end < start) return undefined;
  } else {
    end = start;
    while (end < raw.length && !/\s/.test(raw[end])) end += 1;
  }
  const value = raw.slice(start, end);
  return {
    value: value.includes("&") ? parsedValue : value,
    quoted,
    writable: !value.includes("&"),
    start: location.startOffset + start,
    end: location.startOffset + end,
  };
}
