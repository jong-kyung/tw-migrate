use napi_derive::napi;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrayExpression, ArrayExpressionElement, Expression, ObjectExpression, ObjectPropertyKind,
    PropertyKey, TemplateLiteral,
};
use oxc_parser::Parser;
use oxc_span::Span;
use oxc_syntax::operator::LogicalOperator;

use crate::js_rewrite::source_type_for_path;

#[derive(Debug, PartialEq, Eq)]
#[napi(object)]
pub struct VueClassSite {
    pub value: String,
    pub start: u32,
    pub end: u32,
    /// The authored JS quote delimiter, or `None` for a key that must be quoted
    /// when utilities make it cease to be a valid identifier.
    pub quote: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
#[napi(object)]
pub struct VueClassExpression {
    pub sites: Vec<VueClassSite>,
    pub opaque: bool,
}

pub(crate) fn analyze_vue_class_expression(
    path: &str,
    source: &str,
) -> Result<VueClassExpression, String> {
    let allocator = Allocator::default();
    let source_type = source_type_for_path(path)?;
    let expression = Parser::new(&allocator, source, source_type)
        .parse_expression()
        .map_err(|diagnostics| format!("Failed to parse {path}: {diagnostics:?}"))?;
    let mut analyzer = Analyzer {
        source,
        sites: Vec::new(),
        opaque: false,
    };
    analyzer.expression(&expression);
    Ok(VueClassExpression {
        sites: analyzer.sites,
        opaque: analyzer.opaque,
    })
}

struct Analyzer<'s> {
    source: &'s str,
    sites: Vec<VueClassSite>,
    opaque: bool,
}

impl Analyzer<'_> {
    fn expression(&mut self, expression: &Expression<'_>) {
        match expression.get_inner_expression() {
            Expression::StringLiteral(literal) => {
                self.quoted_site(literal.span, literal.value.as_str())
            }
            Expression::TemplateLiteral(template) => self.template(template),
            Expression::ArrayExpression(array) => self.array(array),
            Expression::ObjectExpression(object) => self.object(object),
            Expression::ConditionalExpression(conditional) => {
                self.expression(&conditional.consequent);
                self.expression(&conditional.alternate);
            }
            Expression::LogicalExpression(logical) if logical.operator == LogicalOperator::And => {
                self.expression(&logical.right);
            }
            // Vue ignores primitive non-string values while normalizing an array.
            Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::NumericLiteral(_)
            | Expression::BigIntLiteral(_) => {}
            _ => self.opaque = true,
        }
    }

    fn array(&mut self, array: &ArrayExpression<'_>) {
        for element in &array.elements {
            match element {
                ArrayExpressionElement::SpreadElement(spread) => {
                    self.array_spread(&spread.argument)
                }
                ArrayExpressionElement::Elision(_) => {}
                element => self.expression(element.as_expression().expect("expression element")),
            }
        }
    }

    fn array_spread(&mut self, expression: &Expression<'_>) {
        match expression.get_inner_expression() {
            Expression::ArrayExpression(array) => self.array(array),
            Expression::ConditionalExpression(conditional) => {
                self.array_spread(&conditional.consequent);
                self.array_spread(&conditional.alternate);
            }
            _ => self.opaque = true,
        }
    }

    fn object(&mut self, object: &ObjectExpression<'_>) {
        for property in &object.properties {
            match property {
                ObjectPropertyKind::ObjectProperty(property) => self.property_key(&property.key),
                ObjectPropertyKind::SpreadProperty(spread) => self.object_spread(&spread.argument),
            }
        }
    }

    fn object_spread(&mut self, expression: &Expression<'_>) {
        match expression.get_inner_expression() {
            Expression::ObjectExpression(object) => self.object(object),
            Expression::ConditionalExpression(conditional) => {
                self.object_spread(&conditional.consequent);
                self.object_spread(&conditional.alternate);
            }
            _ => self.opaque = true,
        }
    }

    fn property_key(&mut self, key: &PropertyKey<'_>) {
        match key {
            PropertyKey::StaticIdentifier(identifier) => self.sites.push(VueClassSite {
                value: identifier.name.to_string(),
                start: identifier.span.start,
                end: identifier.span.end,
                quote: None,
            }),
            key if key.as_expression().is_some() => {
                self.computed_key(key.as_expression().expect("expression key"))
            }
            _ => self.opaque = true,
        }
    }

    fn computed_key(&mut self, expression: &Expression<'_>) {
        match expression.get_inner_expression() {
            Expression::StringLiteral(literal) => {
                self.quoted_site(literal.span, literal.value.as_str())
            }
            Expression::TemplateLiteral(template) => self.template(template),
            Expression::NumericLiteral(literal) => self.sites.push(VueClassSite {
                value: literal.value.to_string(),
                start: literal.span.start,
                end: literal.span.end,
                quote: None,
            }),
            Expression::ConditionalExpression(conditional) => {
                self.computed_key(&conditional.consequent);
                self.computed_key(&conditional.alternate);
            }
            _ => self.opaque = true,
        }
    }

    fn template(&mut self, template: &TemplateLiteral<'_>) {
        if !template.expressions.is_empty() || template.quasis.len() != 1 {
            self.opaque = true;
            return;
        }
        let Some(value) = template.quasis[0].value.cooked.as_ref() else {
            self.opaque = true;
            return;
        };
        self.quoted_site(template.span, value.as_str());
    }

    fn quoted_site(&mut self, span: Span, value: &str) {
        let start = span.start as usize;
        let end = span.end as usize;
        let Some((&quote, content)) = self
            .source
            .as_bytes()
            .get(start..end)
            .and_then(|bytes| bytes.split_first())
        else {
            self.opaque = true;
            return;
        };
        if content.last() != Some(&quote) || !matches!(quote, b'\'' | b'"' | b'`') {
            self.opaque = true;
            return;
        }
        self.sites.push(VueClassSite {
            value: value.to_string(),
            start: span.start + 1,
            end: span.end - 1,
            quote: Some(char::from(quote).to_string()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::analyze_vue_class_expression;

    fn site<'a>(analysis: &'a super::VueClassExpression, value: &str) -> &'a super::VueClassSite {
        analysis
            .sites
            .iter()
            .find(|site| site.value == value)
            .unwrap()
    }

    #[test]
    fn extracts_literal_sites_with_exact_utf8_spans() {
        let source = r#"["기본", { btn: ok, 'active item': yes }, flag ? `wide` : "narrow"]"#;
        let analysis = analyze_vue_class_expression("Component.js", source).unwrap();

        assert!(!analysis.opaque);
        assert_eq!(
            analysis
                .sites
                .iter()
                .map(|site| (site.value.as_str(), site.quote.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("기본", Some("\"")),
                ("btn", None),
                ("active item", Some("'")),
                ("wide", Some("`")),
                ("narrow", Some("\"")),
            ]
        );
        for expected in ["기본", "btn", "active item", "wide", "narrow"] {
            let class_site = site(&analysis, expected);
            let start = class_site.start as usize;
            let end = class_site.end as usize;
            assert_eq!(&source.as_bytes()[start..end], expected.as_bytes());
        }
    }

    #[test]
    fn keeps_proven_siblings_when_fragments_are_opaque() {
        let source =
            "['btn', external, call(), { fixed: cond, [name]: cond }, [...['nested'], ...other]]";
        let analysis = analyze_vue_class_expression("Component.js", source).unwrap();

        assert!(analysis.opaque);
        assert_eq!(
            analysis
                .sites
                .iter()
                .map(|site| site.value.as_str())
                .collect::<Vec<_>>(),
            ["btn", "fixed", "nested"]
        );
    }

    #[test]
    fn supports_literal_spreads_branches_and_typescript_wrappers() {
        let source = "[{ ...(cond ? { yes: ok } : { no: ok }), [pick ? 'a' : 'b']: ok }, flag && (['wide'] as const)]";
        let analysis = analyze_vue_class_expression("Component.ts", source).unwrap();

        assert!(!analysis.opaque);
        assert_eq!(
            analysis
                .sites
                .iter()
                .map(|site| site.value.as_str())
                .collect::<Vec<_>>(),
            ["yes", "no", "a", "b", "wide"]
        );
    }

    #[test]
    fn marks_dynamic_templates_and_concatenation_opaque() {
        let source = "['fixed', `btn-${size}`, 'a' + suffix]";
        let analysis = analyze_vue_class_expression("Component.js", source).unwrap();

        assert!(analysis.opaque);
        assert_eq!(analysis.sites.len(), 1);
        assert_eq!(analysis.sites[0].value, "fixed");
    }
}
