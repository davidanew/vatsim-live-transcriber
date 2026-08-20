fn spoken_digit(word: &str) -> Option<char> {
    Some(match word {
        "zero" | "oh" | "nought" | "nil" => '0',
        "one" => '1',
        "two" => '2',
        "three" | "tree" => '3',
        "four" | "fower" => '4',
        "five" | "fife" => '5',
        "six" => '6',
        "seven" => '7',
        "eight" => '8',
        "nine" | "niner" | "nineer" => '9',
        _ => return None,
    })
}

fn cardinal_value(word: &str) -> Option<u64> {
    if let Some(digit) = spoken_digit(word) {
        return digit.to_digit(10).map(u64::from);
    }
    Some(match word {
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        "thirteen" => 13,
        "fourteen" => 14,
        "fifteen" => 15,
        "sixteen" => 16,
        "seventeen" => 17,
        "eighteen" => 18,
        "nineteen" => 19,
        "twenty" => 20,
        "thirty" => 30,
        "forty" => 40,
        "fifty" => 50,
        "sixty" => 60,
        "seventy" => 70,
        "eighty" => 80,
        "ninety" => 90,
        _ => return None,
    })
}

fn is_scale(word: &str) -> bool {
    matches!(word, "hundred" | "thousand" | "million")
}

fn is_connector(word: &str) -> bool {
    matches!(word, "and" | "decimal" | "point")
}

fn is_repeat(word: &str) -> bool {
    matches!(word, "double" | "triple")
}

fn is_number_word(word: &str) -> bool {
    cardinal_value(word).is_some() || is_scale(word) || is_connector(word) || is_repeat(word)
}

fn is_number_start(word: &str) -> bool {
    cardinal_value(word).is_some() || is_repeat(word)
}

fn expand_repeated_digits(tokens: &[String]) -> Option<Vec<String>> {
    let mut expanded = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if !is_repeat(token) {
            expanded.push(tokens[index].clone());
            index += 1;
            continue;
        }
        let digit = tokens.get(index + 1)?;
        spoken_digit(digit)?;
        let repetitions = if token == "double" { 2 } else { 3 };
        expanded.extend(std::iter::repeat_n(digit.clone(), repetitions));
        index += 2;
    }
    Some(expanded)
}

fn parse_integer_words(tokens: &[String]) -> Option<String> {
    let tokens = tokens
        .iter()
        .map(String::as_str)
        .filter(|token| *token != "and")
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return None;
    }

    if tokens.iter().all(|token| spoken_digit(token).is_some()) {
        return Some(
            tokens
                .iter()
                .filter_map(|token| spoken_digit(token))
                .collect(),
        );
    }

    let mut total = 0_u64;
    let mut current = 0_u64;
    for token in tokens {
        if let Some(value) = cardinal_value(token) {
            current = current.checked_add(value)?;
        } else {
            match token {
                "hundred" => current = current.max(1).checked_mul(100)?,
                "thousand" => {
                    total = total.checked_add(current.max(1).checked_mul(1_000)?)?;
                    current = 0;
                }
                "million" => {
                    total = total.checked_add(current.max(1))?.checked_mul(1_000_000)?;
                    current = 0;
                }
                _ => return None,
            }
        }
    }
    total.checked_add(current).map(|value| value.to_string())
}

fn convert_number_tokens(tokens: &[String]) -> Option<String> {
    let tokens = expand_repeated_digits(tokens)?;
    let decimal_positions = tokens
        .iter()
        .enumerate()
        .filter_map(|(index, token)| matches!(token.as_str(), "decimal" | "point").then_some(index))
        .collect::<Vec<_>>();

    if let [decimal_index] = decimal_positions.as_slice() {
        let whole = parse_integer_words(&tokens[..*decimal_index])?;
        let fraction_tokens = tokens[*decimal_index + 1..]
            .iter()
            .filter(|token| token.as_str() != "and")
            .collect::<Vec<_>>();
        if fraction_tokens.is_empty() {
            return None;
        }
        let fraction = fraction_tokens
            .iter()
            .map(|token| spoken_digit(token))
            .collect::<Option<String>>()?;
        return Some(format!("{whole}.{fraction}"));
    }
    if !decimal_positions.is_empty() {
        return None;
    }

    if tokens.iter().any(|token| token == "and") && !tokens.iter().any(|token| is_scale(token)) {
        let mut groups = vec![Vec::new()];
        for token in tokens {
            if token == "and" {
                groups.push(Vec::new());
            } else {
                groups.last_mut()?.push(token);
            }
        }
        return groups
            .iter()
            .map(|group| parse_integer_words(group))
            .collect::<Option<Vec<_>>>()
            .map(|values| values.join(" and "));
    }

    parse_integer_words(&tokens)
}

fn next_ascii_word(text: &str, from: usize) -> Option<(usize, usize, String)> {
    let bytes = text.as_bytes();
    let mut start = from;
    while start < bytes.len() && !bytes[start].is_ascii_alphabetic() {
        start += 1;
    }
    if start == bytes.len() {
        return None;
    }
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
        end += 1;
    }
    Some((start, end, text[start..end].to_ascii_lowercase()))
}

fn is_number_separator(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_whitespace() || character == '-')
}

/// Render spoken number phrases as digits while preserving all other text.
pub fn normalize_spoken_numbers(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;

    while let Some((start, end, word)) = next_ascii_word(text, cursor) {
        if !is_number_start(&word) {
            output.push_str(&text[cursor..end]);
            cursor = end;
            continue;
        }

        let phrase_start = start;
        let mut phrase_end = end;
        let mut tokens = vec![word];
        let mut token_spans = vec![(start, end)];

        while let Some((next_start, next_end, next_word)) = next_ascii_word(text, phrase_end) {
            if !is_number_separator(&text[phrase_end..next_start]) || !is_number_word(&next_word) {
                break;
            }
            tokens.push(next_word);
            token_spans.push((next_start, next_end));
            phrase_end = next_end;
        }

        let mut trailing_start = phrase_end;
        if tokens.last().is_some_and(|token| is_connector(token)) {
            tokens.pop();
            let (last_start, _) = token_spans.pop().expect("connector span must exist");
            let previous_end = token_spans.last().map_or(phrase_start, |(_, end)| *end);
            trailing_start = previous_end;
            phrase_end = last_start;
        }

        output.push_str(&text[cursor..phrase_start]);
        if let Some(converted) = convert_number_tokens(&tokens) {
            output.push_str(&converted);
            output.push_str(
                &text[trailing_start..token_spans.last().map_or(phrase_end, |(_, end)| *end)],
            );
            if trailing_start < phrase_end {
                output.push_str(&text[trailing_start..phrase_end]);
            }
        } else {
            let actual_end = token_spans.last().map_or(end, |(_, end)| *end);
            output.push_str(&text[phrase_start..actual_end]);
            phrase_end = actual_end;
        }
        cursor = phrase_end;
    }

    output.push_str(&text[cursor..]);
    output
}
