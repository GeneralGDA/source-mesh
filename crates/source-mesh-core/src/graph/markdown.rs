#[must_use]
pub(super) fn inline_code(label: &str) -> String {
    let visible_label = label.replace('\r', "\\r").replace('\n', "\\n").replace('\t', "\\t");
    let mut output = String::from("<code>");
    for character in quick_xml::escape::escape(&visible_label).chars() {
        match character {
            '`' | '*' | '_' | '[' | ']' | '\\' | '~' | '|' => {
                output.push_str("&#");
                output.push_str(&u32::from(character).to_string());
                output.push(';');
            }
            _ => output.push(character),
        }
    }
    output.push_str("</code>");
    output
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::inline_code;

    #[rstest]
    #[case("", "<code></code>")]
    #[case("plain text", "<code>plain text</code>")]
    #[case("&lt;", "<code>&amp;lt;</code>")]
    #[case("\r\n\t", "<code>&#92;r&#92;n&#92;t</code>")]
    #[case("`*_[]\\~|", "<code>&#96;&#42;&#95;&#91;&#93;&#92;&#126;&#124;</code>")]
    fn test_inline_code(#[case] label: &str, #[case] expected: &str) {
        let system_under_test = inline_code;

        let output = system_under_test(label);

        assert_eq!(expected, output, "Inline code escaping changed");
    }
}
