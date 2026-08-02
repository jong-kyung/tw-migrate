import { parse } from "parse5";

const RECOVERABLE_PARSE_ERRORS = new Set(["missing-doctype"]);
const TEMPLATE_MARKERS = /\{\{|\}\}|\$\{|<%|%>|<#|#>|\[%|%\]/;

// Minimal structural view of the parse5 tree; the walk duck-types nodes the
// same way the untyped implementation did.
interface ParsedAttribute {
  name: string;
  value: string;
}

interface OffsetRange {
  startOffset: number;
  endOffset: number;
}

interface NodeLocation extends OffsetRange {
  attrs?: Record<string, OffsetRange | undefined>;
  startTag?: OffsetRange;
}

interface HtmlNode {
  nodeName: string;
  tagName?: string;
  value?: string;
  attrs?: ParsedAttribute[];
  childNodes?: HtmlNode[];
  content?: HtmlNode;
  sourceCodeLocation?: NodeLocation | null;
}

export interface HtmlAttribute {
  value: string;
  start: number;
  end: number;
  writable?: boolean;
  synthetic?: boolean;
}

export interface HtmlLink {
  href: string;
  media: string;
  start: number;
  end: number;
  tagStart: number;
  tagEnd: number;
}

export interface HtmlBase {
  href: string;
  writable?: boolean;
  start: number;
  end: number;
}

export interface HtmlElementAttributes {
  classAttribute?: HtmlAttribute;
  idAttribute?: HtmlAttribute;
}

export interface HtmlSpan {
  start: number;
  end: number;
}

export interface ParsedHtml {
  links: HtmlLink[];
  bases: HtmlBase[];
  elements: HtmlElementAttributes[];
  dynamicAttributes: HtmlSpan[];
  scriptText: string;
}

export function parseHtmlSource(path: string, source: string): ParsedHtml {
  const errors: { code: string }[] = [];
  const document = parse(source, {
    sourceCodeLocationInfo: true,
    onParseError(error) {
      if (!RECOVERABLE_PARSE_ERRORS.has(error.code)) errors.push(error);
    },
  });
  if (errors.length > 0) {
    throw new Error(`Failed to parse ${path}: ${errors.map((error) => error.code).join(", ")}`);
  }

  const links: HtmlLink[] = [];
  const bases: HtmlBase[] = [];
  const elements: HtmlElementAttributes[] = [];
  const dynamicAttributes: HtmlSpan[] = [];
  const scriptTexts: string[] = [];
  // The value span of a quoted attribute starts right after its quote, so the
  // preceding character distinguishes quoted from unquoted values.
  const unquoted = (attribute: HtmlAttribute): boolean =>
    source[attribute.start - 1] !== '"' && source[attribute.start - 1] !== "'";

  function visit(node: HtmlNode): void {
    if (node.tagName === "script") {
      for (const child of node.childNodes ?? []) {
        if (child.nodeName === "#text" && child.value) scriptTexts.push(child.value);
      }
    }
    if (node.tagName) {
      const attributes = new Map(
        (node.attrs ?? []).map((attribute) => [attribute.name, attribute.value]),
      );
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
          (classAttribute && unquoted(classAttribute) ? classAttribute : undefined);
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
export function utf8OffsetMap(source: string, indices: number[]): Map<number, number> {
  const sorted = [...new Set(indices)].sort((left, right) => left - right);
  const map = new Map<number, number>();
  let lastIndex = 0;
  let bytes = 0;
  for (const index of sorted) {
    bytes += Buffer.byteLength(source.slice(lastIndex, index));
    lastIndex = index;
    map.set(index, bytes);
  }
  return map;
}

function toByteOffsets(
  source: string,
  parsed: Omit<ParsedHtml, "scriptText">,
): Omit<ParsedHtml, "scriptText"> {
  const offsets = utf8OffsetMap(source, [
    ...parsed.links.flatMap((link) => [link.start, link.end, link.tagStart, link.tagEnd]),
    ...parsed.bases.flatMap((base) => [base.start, base.end]),
    ...parsed.elements.flatMap((element) =>
      [element.classAttribute, element.idAttribute].flatMap((value) =>
        value ? [value.start, value.end] : [],
      ),
    ),
    ...parsed.dynamicAttributes.flatMap((value) => [value.start, value.end]),
  ]);
  // Every queried index was fed into the map above, so a miss is a bug in
  // this module rather than a recoverable input condition.
  const offset = (index: number): number => {
    const byte = offsets.get(index);
    if (byte === undefined) throw new Error(`No byte offset was mapped for index ${index}`);
    return byte;
  };
  const attribute = <T extends HtmlSpan>(value: T): T => ({
    ...value,
    start: offset(value.start),
    end: offset(value.end),
  });
  return {
    links: parsed.links.map((link) => ({
      ...link,
      start: offset(link.start),
      end: offset(link.end),
      tagStart: offset(link.tagStart),
      tagEnd: offset(link.tagEnd),
    })),
    bases: parsed.bases.map((base) => attribute(base)),
    elements: parsed.elements.map((element) => ({
      classAttribute: element.classAttribute && attribute(element.classAttribute),
      idAttribute: element.idAttribute && attribute(element.idAttribute),
    })),
    dynamicAttributes: parsed.dynamicAttributes.map((value) => attribute(value)),
  };
}

function classInsertionOffset(source: string, startTag?: OffsetRange): number | undefined {
  if (!startTag) return undefined;
  let offset = startTag.endOffset - 1;
  if (source[offset] !== ">") return undefined;
  while (offset > startTag.startOffset && /\s/.test(source[offset - 1] ?? "")) offset -= 1;
  if (source[offset - 1] === "/") offset -= 1;
  return offset;
}

function stylesheetRel(value = ""): boolean {
  const tokens = value.toLowerCase().split(/\s+/);
  return tokens.includes("stylesheet") && !tokens.includes("alternate");
}

function locatedAttribute(
  source: string,
  location: OffsetRange | undefined,
  parsedValue: string | undefined,
): HtmlAttribute | undefined {
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
    while (end < raw.length && !/\s/.test(raw[end] ?? "")) end += 1;
  }
  const value = raw.slice(start, end);
  return {
    value: value.includes("&") ? parsedValue : value,
    writable: !value.includes("&"),
    start: location.startOffset + start,
    end: location.startOffset + end,
  };
}
