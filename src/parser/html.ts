import { parse, type Token } from "parse5";

export const TEMPLATE_MARKERS = /\{\{|\}\}|\$\{|<%|%>|<#|#>|\[%|%\]/;

/** Balanced template expressions, for masking a templated attribute value
 * before extracting its static class-prefix tokens. */
export const TEMPLATE_EXPRESSIONS =
  /\{\{[\s\S]*?\}\}|\$\{[\s\S]*?\}|<%[\s\S]*?%>|<#[\s\S]*?#>|\[%[\s\S]*?%\]/g;

// Minimal structural view of the parse5 tree; the walk duck-types nodes the
// same way the untyped implementation did.
interface HtmlNode {
  nodeName: string;
  tagName?: string;
  value?: string;
  attrs?: Token.Attribute[];
  childNodes?: HtmlNode[];
  content?: HtmlNode;
  sourceCodeLocation?: Token.ElementLocation | null;
}

interface HtmlAttribute {
  value: string;
  start: number;
  end: number;
  writable?: boolean;
  synthetic?: boolean;
}

interface HtmlLink {
  href: string;
  media: string;
  start: number;
  end: number;
  tagStart: number;
  tagEnd: number;
}

interface HtmlBase {
  href: string;
  writable?: boolean;
  start: number;
  end: number;
}

export interface HtmlElementAttributes {
  classAttribute?: HtmlAttribute;
  idAttribute?: HtmlAttribute;
}

interface HtmlSpan {
  start: number;
  end: number;
}

interface ParsedHtml {
  links: HtmlLink[];
  bases: HtmlBase[];
  elements: HtmlElementAttributes[];
  dynamicAttributes: HtmlSpan[];
  /** Decoded values of dynamic class attributes, so spelling
   * reservations can extract template-constrained prefixes. */
  dynamicClassValues: string[];
  /** Decoded values of style attributes, whose inline declarations can
   * override custom properties. */
  styleAttributeValues: string[];
  /** Decoded inline event-handler bodies (onclick and friends), analyzed
   * like inline script text for reservations. */
  handlerText: string;
  scriptText: string;
  hasStyle: boolean;
}

export function parseHtmlSource(path: string, source: string): ParsedHtml {
  const errors: { code: string }[] = [];
  const document = parse(source, {
    sourceCodeLocationInfo: true,
    // Without scripting, `<noscript>` content parses as real elements, so
    // stylesheet links and style tags inside it keep defeating deletion
    // proofs the way the browser's no-JS fallback rendering would.
    scriptingEnabled: false,
    onParseError(error) {
      if (error.code !== "missing-doctype") errors.push(error);
    },
  });
  if (errors.length > 0) {
    throw new Error(`Failed to parse ${path}: ${errors.map((error) => error.code).join(", ")}`);
  }

  const links: HtmlLink[] = [];
  const bases: HtmlBase[] = [];
  const elements: HtmlElementAttributes[] = [];
  const dynamicAttributes: HtmlSpan[] = [];
  const dynamicClassValues: string[] = [];
  const styleAttributeValues: string[] = [];
  const handlerTexts: string[] = [];
  const scriptTexts: string[] = [];
  let hasStyle = false;
  // The value span of a quoted attribute starts right after its quote, so the
  // preceding character distinguishes quoted from unquoted values.
  const unquoted = (attribute: HtmlAttribute): boolean =>
    source[attribute.start - 1] !== '"' && source[attribute.start - 1] !== "'";

  function visit(node: HtmlNode): void {
    if (node.tagName === "style") hasStyle = true;
    if (node.tagName === "script") {
      for (const child of node.childNodes ?? []) {
        if (child.nodeName === "#text" && child.value) scriptTexts.push(child.value);
      }
    }
    if (node.tagName) {
      const attributes = new Map(
        (node.attrs ?? []).map((attribute) => [attribute.name, attribute.value]),
      );
      const style = attributes.get("style");
      if (style !== undefined && style !== "") styleAttributeValues.push(style);
      for (const [name, value] of attributes) {
        if (name.startsWith("on") && value !== "") handlerTexts.push(value);
      }
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
          // Templated, entity-bearing, and unlocatable class values are
          // invisible to Tailwind's raw-text scan, so their decoded forms
          // feed spelling reservations. A clean unquoted value reads the
          // same raw and decoded, so the scanner already covers it.
          const decoded = attributes.get("class");
          const entityBearing =
            classAttribute !== undefined &&
            dynamic === classAttribute &&
            classAttribute.writable === false;
          if (
            dynamic !== idAttribute &&
            decoded !== undefined &&
            (TEMPLATE_MARKERS.test(decoded) || entityBearing || dynamic === unparsedClass)
          ) {
            dynamicClassValues.push(decoded);
          }
        } else if (classAttribute || idAttribute) {
          if (!classAttribute && idAttribute) {
            const startTag = node.sourceCodeLocation?.startTag;
            if (startTag && source[startTag.endOffset - 1] === ">") {
              const insertion = classInsertionOffset(
                source,
                startTag.startOffset,
                startTag.endOffset - 1,
              );
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
    dynamicClassValues,
    styleAttributeValues,
    handlerText: handlerTexts.join(";\n"),
    scriptText: scriptTexts.join("\n"),
    hasStyle,
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

// Every queried index was fed into the offset map, so a miss is a bug in the
// calling module rather than a recoverable input condition.
export function offsetLookup(offsets: Map<number, number>): (index: number) => number {
  return (index) => {
    const byte = offsets.get(index);
    if (byte === undefined) throw new Error(`No byte offset was mapped for index ${index}`);
    return byte;
  };
}

function toByteOffsets(
  source: string,
  parsed: Omit<
    ParsedHtml,
    "dynamicClassValues" | "styleAttributeValues" | "handlerText" | "scriptText" | "hasStyle"
  >,
): Omit<
  ParsedHtml,
  "dynamicClassValues" | "styleAttributeValues" | "handlerText" | "scriptText" | "hasStyle"
> {
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
  const offset = offsetLookup(offsets);
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

// Walk back from the `>` at tagEnd across a self-closing `/` and trailing
// whitespace to where a synthesized class attribute can be inserted. Shared
// with the Vue template planner so both migrate `<br />` the same way.
export function classInsertionOffset(source: string, floor: number, tagEnd: number): number {
  let offset = tagEnd;
  if (source[offset - 1] === "/") offset -= 1;
  while (offset > floor && /\s/.test(source[offset - 1] ?? "")) offset -= 1;
  return offset;
}

function stylesheetRel(value = ""): boolean {
  const tokens = value.toLowerCase().split(/\s+/);
  return tokens.includes("stylesheet") && !tokens.includes("alternate");
}

function locatedAttribute(
  source: string,
  location: Token.Location | undefined,
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
