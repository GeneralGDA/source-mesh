use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use getset::CopyGetters;

#[derive(PartialEq, Eq, Hash, CopyGetters)]
pub(crate) struct ScipRange {
    #[getset(get_copy = "pub(crate)")]
    start: (u32, u32),
    end: (u32, u32),
}

impl ScipRange {
    pub(crate) fn parse(coordinates: &[i32]) -> Result<Self> {
        let (start, end) = match *coordinates {
            [line, start_column, end_column] => ((line, start_column), (line, end_column)),
            [start_line, start_column, end_line, end_column] => ((start_line, start_column), (end_line, end_column)),
            _ => bail!("SCIP range must contain three or four coordinates: {coordinates:?}"),
        };
        let unsigned_coordinate = |coordinate: i32| {
            u32::try_from(coordinate).with_context(|| format!("SCIP range must contain nonnegative positions: {coordinates:?}"))
        };
        let start = (unsigned_coordinate(start.0)?, unsigned_coordinate(start.1)?);
        let end = (unsigned_coordinate(end.0)?, unsigned_coordinate(end.1)?);
        ensure!(start <= end, "SCIP range must contain ordered positions: {coordinates:?}");
        Ok(Self { start, end })
    }

    #[must_use]
    pub(crate) fn contains(&self, range: &Self) -> bool {
        self.start <= range.start && range.end <= self.end && range.start < range.end
    }
}

#[cfg(test)]
mod tests {
    use claims::assert_ok;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::ScipRange;

    #[rstest]
    #[case::origin(&[0, 0, 1], (0, 0), (0, 1))]
    #[case::single_line(&[7, 13, 17], (7, 13), (7, 17))]
    #[case::explicit_single_line(&[7, 13, 7, 17], (7, 13), (7, 17))]
    #[case::multiple_lines(&[7, 13, 8, 2], (7, 13), (8, 2))]
    #[case::empty_origin(&[0, 0, 0], (0, 0), (0, 0))]
    #[case::empty_single_line(&[7, 13, 13], (7, 13), (7, 13))]
    #[case::empty_explicit_single_line(&[7, 13, 7, 13], (7, 13), (7, 13))]
    #[case::maximum_single_line(&[i32::MAX, 0, i32::MAX], (2_147_483_647, 0), (2_147_483_647, 2_147_483_647))]
    #[case::maximum_multiple_lines(&[0, 0, i32::MAX, i32::MAX], (0, 0), (2_147_483_647, 2_147_483_647))]
    #[case::maximum_empty(&[i32::MAX, i32::MAX, i32::MAX], (2_147_483_647, 2_147_483_647), (2_147_483_647, 2_147_483_647),)]
    fn test_range_coordinates(#[case] coordinates: &[i32], #[case] expected_start: (u32, u32), #[case] expected_end: (u32, u32)) {
        let system_under_test = ScipRange::parse(coordinates);
        let system_under_test = assert_ok!(system_under_test, "Valid SCIP coordinates must parse: {coordinates:?}");

        let positions = (system_under_test.start(), system_under_test.end);

        assert_eq!(positions, (expected_start, expected_end), "Incorrect SCIP range coordinates: {coordinates:?}");
    }

    #[rstest]
    #[case::nonempty(&[7, 13, 17], &[7, 13, 7, 17])]
    #[case::empty(&[7, 13, 13], &[7, 13, 7, 13])]
    fn test_equivalent_range_encodings(#[case] compact: &[i32], #[case] explicit: &[i32]) {
        let fixture = ScipRange::parse(explicit);
        let system_under_test = ScipRange::parse(compact);
        let system_under_test = assert_ok!(system_under_test, "The compact SCIP range must parse: {compact:?}");
        let fixture = assert_ok!(fixture, "The explicit SCIP range must parse: {explicit:?}");

        let equivalent = system_under_test == fixture;

        assert!(equivalent, "Equivalent SCIP range encodings must have the same identity");
    }

    #[rstest]
    #[case(&[3, 0, 1], true)]
    #[case(&[12, 0, 1], true)]
    #[case(&[4, 0, 8, 1], true)]
    #[case(&[3, 0, 12, 1], true)]
    #[case(&[12, 1, 2], false)]
    #[case(&[2, 0, 3], false)]
    #[case(&[2, 0, 4, 0], false)]
    #[case(&[11, 0, 12, 2], false)]
    #[case(&[7, 0, 0], false)]
    #[case(&[7, 0, 7, 0], false)]
    fn test_scope_range_containment(#[case] coordinates: &[i32], #[case] expected: bool) {
        let reference_range = ScipRange::parse(coordinates);
        let system_under_test = ScipRange::parse(&[3, 0, 12, 1]);
        let system_under_test = assert_ok!(system_under_test, "The enclosing scope range must parse");
        let reference_range = assert_ok!(reference_range, "The reference range must parse: {coordinates:?}");

        let contains_reference = system_under_test.contains(&reference_range);

        assert_eq!(contains_reference, expected, "Scope containment was incorrect for {coordinates:?}");
    }

    #[rstest]
    #[case::empty(&[])]
    #[case::one_coordinate(&[0])]
    #[case::two_coordinates(&[0, 0])]
    #[case::five_coordinates(&[0, 0, 1, 0, 1])]
    #[case::negative_line(&[-1, 0, 3])]
    #[case::negative_start_column(&[3, -1, 4])]
    #[case::negative_columns(&[3, -2, -1])]
    #[case::negative_end_column(&[3, 0, -1])]
    #[case::negative_explicit_start_line(&[-1, 0, 4, 0])]
    #[case::negative_explicit_start_column(&[3, -1, 4, 0])]
    #[case::negative_explicit_end_line(&[3, 0, -1, 0])]
    #[case::negative_explicit_end_column(&[3, 0, 4, -1])]
    #[case::reversed_columns(&[0, 4, 3])]
    #[case::reversed_lines(&[4, 0, 3, 0])]
    #[case::reversed_explicit_columns(&[3, 4, 3, 0])]
    #[case::minimum_coordinate(&[i32::MIN, 0, 3])]
    fn test_invalid_scope_ranges(#[case] coordinates: &[i32]) {
        let expected_coordinates = format!("{coordinates:?}");

        let system_under_test = ScipRange::parse(coordinates);
        let diagnostic = system_under_test.err().map(|error| error.to_string());

        assert!(
            diagnostic.is_some_and(|message| message.contains("SCIP range") && message.contains(&expected_coordinates)),
            "Invalid SCIP ranges must retain format and coordinate context"
        );
    }
}
