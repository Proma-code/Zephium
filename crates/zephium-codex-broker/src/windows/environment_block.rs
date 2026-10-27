//! Whole-block resource admission only. The caller supplies separately owned
//! runtime entries plus the fixed transport allowlist, never generic inheritance.
const MAX_UNITS: usize = 65_536;

pub(super) fn encode(
    entries: impl IntoIterator<Item = Result<Vec<u16>, ()>>,
) -> Result<Vec<u16>, ()> {
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.last() != Some(&0)
            || entry.len() < 3
            || entry[..entry.len() - 1].contains(&0)
            || entry[..entry.len() - 1]
                .iter()
                .position(|unit| *unit == u16::from(b'='))
                .is_none_or(|position| position == 0)
            || out
                .len()
                .checked_add(entry.len())
                .and_then(|n| n.checked_add(1))
                .is_none_or(|total| total > MAX_UNITS)
        {
            return Err(());
        }
        out.extend(entry);
    }
    // Every admitted entry already ends in NUL; this is the block terminator.
    // Native callers always supply the ten owned entries, never an empty set.
    if out.is_empty() {
        return Err(());
    }
    out.push(0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_terminator_is_counted_at_the_exact_combined_bound() {
        let mut entry = vec![u16::from(b'x'); MAX_UNITS - 1];
        entry[1] = u16::from(b'=');
        *entry.last_mut().unwrap() = 0;
        let block = encode([Ok(entry.clone())]).unwrap();
        assert_eq!(block.len(), MAX_UNITS);
        assert_eq!(&block[MAX_UNITS - 2..], &[0, 0]);
        entry.insert(2, u16::from(b'x'));
        assert!(encode([Ok(entry)]).is_err());
    }
    #[test]
    fn empty_values_and_non_unicode_units_are_not_normalized() {
        let entries = [vec![65, 61, 0], vec![66, 61, 0xd800, 61, 0]];
        assert_eq!(
            encode(entries.map(Ok)).unwrap(),
            [65, 61, 0, 66, 61, 0xd800, 61, 0, 0]
        );
    }
    #[test]
    fn bad_or_overflowing_entry_refuses_the_whole_block() {
        for entry in [
            vec![],
            vec![0],
            vec![61, 65, 0],
            vec![65, 0, 61, 0],
            vec![65, 61],
        ] {
            assert!(encode([Ok(vec![65, 61, 0]), Ok(entry)]).is_err());
        }
        assert!(encode([Ok(vec![65, 61, 0]), Err(())]).is_err());
        assert!(encode([]).is_err());
        let mut large = vec![65; MAX_UNITS - 4];
        large[1] = 61;
        *large.last_mut().unwrap() = 0;
        assert!(encode([Ok(large), Ok(vec![66, 61, 67, 0])]).is_err());
    }
}
