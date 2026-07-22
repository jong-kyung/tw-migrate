import { parse } from 'parse5';

const RECOVERABLE_PARSE_ERRORS = new Set(['missing-doctype']);
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
    throw new Error(`Failed to parse ${path}: ${errors.map((error) => error.code).join(', ')}`);
  }

  const links = [];
  const bases = [];
  const elements = [];
  const dynamicAttributes = [];

  function visit(node) {
    if (node.tagName) {
      const attributes = new Map(node.attrs.map((attribute) => [attribute.name, attribute.value]));
      const locations = node.sourceCodeLocation?.attrs;
      if (locations) {
        if (node.tagName === 'link'
          && stylesheetRel(attributes.get('rel'))
          && !attributes.has('disabled')) {
          const href = locatedAttribute(source, locations.href, attributes.get('href'));
          const media = locatedAttribute(source, locations.media, attributes.get('media'));
          const tag = node.sourceCodeLocation?.startTag ?? node.sourceCodeLocation;
          if (href && tag) {
            links.push({
              href: href.value,
              media: media?.value ?? '',
              start: href.start,
              end: href.end,
              tagStart: tag.startOffset,
              tagEnd: tag.endOffset,
            });
          }
        }
        if (node.tagName === 'base' && bases.length === 0) {
          const href = locatedAttribute(source, locations.href, attributes.get('href'));
          if (href) bases.push({ href: href.value, writable: href.writable, start: href.start, end: href.end });
        }

        let classAttribute = locatedAttribute(source, locations.class, attributes.get('class'));
        const idAttribute = locatedAttribute(source, locations.id, attributes.get('id'));
        // A class attribute that exists but cannot be located as a writable
        // value (e.g. valueless `<main class>`) must not look absent, or the
        // id-only branch would synthesize a duplicate class attribute.
        const unparsedClass = !classAttribute && idAttribute && locations.class
          ? { start: locations.class.startOffset, end: locations.class.endOffset }
          : undefined;
        const dynamic = unparsedClass
          ?? [classAttribute, idAttribute].find((attribute) =>
            attribute && (!attribute.writable || isTemplateValue(attribute.value)),
          ) ?? (classAttribute && !classAttribute.quoted ? classAttribute : undefined);
        if (dynamic) {
          dynamicAttributes.push({ start: dynamic.start, end: dynamic.end });
        } else if (classAttribute || idAttribute) {
          if (!classAttribute && idAttribute) {
            const insertion = classInsertionOffset(source, node.sourceCodeLocation?.startTag);
            if (insertion !== undefined) {
              classAttribute = { value: '', start: insertion, end: insertion, synthetic: true };
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
  return toByteOffsets(source, { links, bases, elements, dynamicAttributes });
}

function toByteOffsets(source, parsed) {
  const offset = (index) => Buffer.byteLength(source.slice(0, index));
  const attribute = (value) => value && { ...value, start: offset(value.start), end: offset(value.end) };
  return {
    links: parsed.links.map((link) => ({
      ...link,
      start: offset(link.start),
      end: offset(link.end),
      tagStart: offset(link.tagStart),
      tagEnd: offset(link.tagEnd),
    })),
    bases: parsed.bases.map((base) => ({ ...base, start: offset(base.start), end: offset(base.end) })),
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
  if (source[offset] !== '>') return undefined;
  while (offset > startTag.startOffset && /\s/.test(source[offset - 1])) offset -= 1;
  if (source[offset - 1] === '/') offset -= 1;
  return offset;
}

function stylesheetRel(value = '') {
  const tokens = value.toLowerCase().split(/\s+/);
  return tokens.includes('stylesheet') && !tokens.includes('alternate');
}

function locatedAttribute(source, location, parsedValue) {
  if (!location || parsedValue === undefined) return undefined;
  const raw = source.slice(location.startOffset, location.endOffset);
  const equals = raw.indexOf('=');
  if (equals < 0) return undefined;
  let start = equals + 1;
  while (/\s/.test(raw[start] ?? '')) start += 1;
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
    value: value.includes('&') ? parsedValue : value,
    quoted,
    writable: !value.includes('&'),
    start: location.startOffset + start,
    end: location.startOffset + end,
  };
}

function isTemplateValue(value) {
  return TEMPLATE_MARKERS.test(value);
}
