use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;

use colorous::Color;

#[derive(Clone, Copy)]
pub(crate) struct RgbColor {
    color: Color,
}

impl RgbColor {
    #[must_use]
    pub(crate) const fn new(color: Color) -> Self {
        Self { color }
    }
}

impl Display for RgbColor {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "#{:X}", self.color)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::io::Write as _;

    use claims::assert_ok;
    use colorous::Color;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::RgbColor;

    #[rstest]
    #[case::black(Color { r: 0, g: 0, b: 0 }, "#000000")]
    #[case::white(Color { r: 255, g: 255, b: 255 }, "#FFFFFF")]
    #[case::red(Color { r: 255, g: 0, b: 0 }, "#FF0000")]
    #[case::green(Color { r: 0, g: 255, b: 0 }, "#00FF00")]
    #[case::blue(Color { r: 0, g: 0, b: 255 }, "#0000FF")]
    #[case::mixed(Color { r: 0x0A, g: 0xB0, b: 0x0C }, "#0AB00C")]
    fn test_rgb_hex_format(#[case] color: Color, #[case] expected: &str) {
        let system_under_test = RgbColor::new(color);
        let mut output = Cursor::new([0; b"#RRGGBB".len()]);

        let result = write!(output, "{system_under_test}");

        assert_ok!(result, "RGB color must fit in the fixed-size hex output buffer");
        assert_eq!(output.into_inner().as_slice(), expected.as_bytes(), "RGB hex formatting changed");
    }

    #[test]
    fn test_write_failure() {
        let system_under_test = RgbColor::new(Color { r: 0, g: 0, b: 0 });
        let mut output = Cursor::new([]);

        let result = write!(output, "{system_under_test}");

        assert!(result.is_err(), "Color formatting must propagate output errors");
    }
}
