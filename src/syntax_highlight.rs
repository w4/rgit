use std::{collections::HashMap, io::Write};

use comrak::adapters::SyntaxHighlighterAdapter;
use syntect::{
    html::{ClassStyle, ClassedHTMLGenerator},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

pub struct ComrakSyntectAdapter<'a> {
    pub(crate) syntax_set: &'a SyntaxSet,
}

impl SyntaxHighlighterAdapter for ComrakSyntectAdapter<'_> {
    fn write_highlighted(
        &self,
        output: &mut dyn Write,
        lang: Option<&str>,
        code: &str,
    ) -> std::io::Result<()> {
        let syntax = lang
            .and_then(|v| self.syntax_set.find_syntax_by_token(v))
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let mut html_generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, self.syntax_set, ClassStyle::Spaced);

        for line in LinesWithEndings::from(code) {
            let _res = html_generator.parse_html_for_line_which_includes_newline(line);
        }

        write!(
            output,
            "<code>{}</code>",
            html_generator.finalize().replace('\n', "</code>\n<code>")
        )
    }

    fn write_pre_tag(
        &self,
        output: &mut dyn Write,
        _attributes: HashMap<String, String>,
    ) -> std::io::Result<()> {
        write!(output, r#"<pre>"#)
    }

    fn write_code_tag(
        &self,
        _output: &mut dyn Write,
        _attributes: HashMap<String, String>,
    ) -> std::io::Result<()> {
        Ok(())
    }
}
