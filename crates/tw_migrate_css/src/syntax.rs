use oxc_css_parser::Syntax;
use serde::Deserialize;

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StylesheetSyntax {
    #[default]
    Css,
    Scss,
    Sass,
    Less,
}

impl StylesheetSyntax {
    pub fn parser_syntax(self) -> Syntax {
        match self {
            Self::Css => Syntax::Css,
            Self::Scss => Syntax::Scss,
            Self::Sass => Syntax::Sass,
            Self::Less => Syntax::Less,
        }
    }
}
