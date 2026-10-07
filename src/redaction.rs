use aho_corasick::{Anchored, automaton::Automaton, dfa::DFA};
use anyhow::Result;
use serde_json::Value;

pub struct Redactor {
    matcher: DFA,
    prefixes: Vec<(Vec<u8>, Vec<usize>)>,
}
impl Redactor {
    pub fn new(secrets: &[String]) -> Result<Self> {
        let mut patterns: Vec<_> = secrets.iter().filter(|s| !s.is_empty()).collect();
        patterns.sort_unstable();
        patterns.dedup();
        let matcher = DFA::builder()
            .build(&patterns)
            .map_err(|_| anyhow::anyhow!("Cannot build credential redactor"))?;
        let prefixes = patterns
            .iter()
            .map(|secret| {
                let bytes = secret.as_bytes().to_vec();
                let mut table = vec![0; bytes.len()];
                for i in 1..bytes.len() {
                    let mut n = table[i - 1];
                    while n > 0 && bytes[n] != bytes[i] {
                        n = table[n - 1];
                    }
                    if bytes[n] == bytes[i] {
                        n += 1;
                    }
                    table[i] = n;
                }
                (bytes, table)
            })
            .collect();
        Ok(Self { matcher, prefixes })
    }
    pub fn bytes(&self, input: &[u8], truncated: bool) -> Vec<u8> {
        if self.prefixes.is_empty() {
            return input.to_vec();
        }
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut mark = |mut start: usize, end: usize| {
            while ranges.last().is_some_and(|range| range.1 >= start) {
                start = start.min(ranges.pop().unwrap().0);
            }
            ranges.push((start, end));
        };
        let mut lengths = std::collections::HashMap::new();
        let mut state = self.matcher.start_state(Anchored::No).unwrap();
        for (i, &byte) in input.iter().enumerate() {
            state = self.matcher.next_state(Anchored::No, state, byte);
            if self.matcher.is_match(state) {
                let length = *lengths.entry(state).or_insert_with(|| {
                    (0..self.matcher.match_len(state))
                        .map(|n| {
                            self.matcher
                                .pattern_len(self.matcher.match_pattern(state, n))
                        })
                        .max()
                        .unwrap()
                });
                mark(i + 1 - length, i + 1);
            }
        }
        if truncated {
            let tail = self
                .prefixes
                .iter()
                .map(|(pattern, table)| {
                    let mut n = 0;
                    for &byte in &input[input.len().saturating_sub(pattern.len())..] {
                        if n == pattern.len() {
                            n = table[n - 1];
                        }
                        while n > 0 && pattern[n] != byte {
                            n = table[n - 1];
                        }
                        if pattern[n] == byte {
                            n += 1;
                        }
                    }
                    n
                })
                .max()
                .unwrap_or(0);
            if tail > 0 {
                mark(input.len() - tail, input.len());
            }
        }
        let mut output = Vec::new();
        let mut copied = 0;
        for (start, end) in ranges {
            output.extend_from_slice(&input[copied..start]);
            output.extend_from_slice(b"[REDACTED]");
            copied = end;
        }
        output.extend_from_slice(&input[copied..]);
        output
    }
    pub fn value(&self, value: &mut Value) {
        match value {
            Value::String(text) => {
                *text = String::from_utf8(self.bytes(text.as_bytes(), false)).unwrap()
            }
            Value::Array(values) => {
                for value in values {
                    self.value(value);
                }
            }
            Value::Object(values) => {
                let mut suffix = 0;
                for (key, mut value) in std::mem::take(values) {
                    let base = String::from_utf8(self.bytes(key.as_bytes(), false)).unwrap();
                    let mut key = base.clone();
                    self.value(&mut value);
                    while values.contains_key(&key) {
                        suffix += 1;
                        key = format!("{base} [{suffix}]");
                    }
                    values.insert(key, value);
                }
            }
            Value::Number(number) => {
                let text = number.to_string();
                let redacted = self.bytes(text.as_bytes(), false);
                if redacted != text.as_bytes() {
                    *value = Value::String(String::from_utf8(redacted).unwrap());
                }
            }
            _ => {}
        }
    }
}
pub fn redact_value(value: &mut Value, secrets: &[String]) {
    match Redactor::new(secrets) {
        Ok(redactor) => redactor.value(value),
        Err(_) => *value = Value::String("[REDACTED]".into()),
    }
}
