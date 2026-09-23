//! PRINT message policy after source conversion. Counts are UTF-16 units;
//! ANSI conversion has already established one unit per code-page byte.
pub fn message(source: Option<&[u16]>, unicode: bool) -> &[u16] {
    match source {
        None | Some([]) => &[32],
        Some(units) => &units[..units.len().min(if unicode { 4000 } else { 8000 })],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_empty_null_nul_and_surrogate_boundaries() {
        assert_eq!(message(None, true), [32]);
        assert_eq!(message(Some(&[]), false), [32]);
        assert_eq!(message(Some(&[97, 0, 98]), true), [97, 0, 98]);
        let mut source = vec![120; 3999];
        source.extend([0xd83e, 0xdd86]);
        let result = message(Some(&source), true);
        assert_eq!(result.len(), 4000);
        assert_eq!(result[3999], 0xd83e);
        source.remove(0);
        assert_eq!(&message(Some(&source), true)[3998..], [0xd83e, 0xdd86]);
        assert_eq!(message(Some(&vec![120; 8100]), false).len(), 8000);
    }
}
