fn edit_distance(x: &str, y: &str) -> usize {
    let x_len = x.chars().count();

    let mut prev = (0..=x_len).collect::<Vec<_>>();
    let mut current;

    for (j, y_char) in y.char_indices() {
        current = Some(j + 1)
            .into_iter()
            .chain(0..)
            .take(x_len + 1)
            .collect::<Vec<_>>();
        for (i, x_char) in x.char_indices() {
            current[i + 1] = if y_char == x_char {
                prev[i]
            } else {
                std::cmp::min(
                    prev[i] + 1, // replacement
                    std::cmp::min(
                        current[i] + 1,  // insertion
                        prev[i + 1] + 1, // deletion
                    ),
                )
            };
        }
        prev = current;
    }

    *prev.last().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic() {
        assert_eq!(0, edit_distance("", ""));
        assert_eq!(3, edit_distance("abc", ""));
        assert_eq!(3, edit_distance("", "abc"));
        assert_eq!(2, edit_distance("ab", "cd"));
        assert_eq!(1, edit_distance("car", "cat"));
        assert_eq!(4, edit_distance("hello", "world"));
    }
}
