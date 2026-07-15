//! Repairs structural GLM reasoning delimiters and preserves channel boundaries.
//! This module deliberately does not score semantic quality or classify prose.

const MARKERS: [(&str, bool); 4] = [
    ("<think>", true),
    ("</think>", false),
    ("<thinking>", true),
    ("</thinking>", false),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Reasoning,
    Content,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BoundaryOutput {
    pub reasoning: String,
    pub content: String,
}

impl BoundaryOutput {
    pub(crate) fn append(&mut self, other: Self) {
        self.reasoning.push_str(&other.reasoning);
        self.content.push_str(&other.content);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.reasoning.is_empty() && self.content.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BoundaryError {
    #[error("reasoning boundary buffer limit exceeded")]
    BufferOverflow,
    #[error("malformed reasoning boundary")]
    Malformed,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Unresolved,
    Content,
    Done,
}

#[derive(Default)]
struct Held {
    text: String,
    opener_bytes: usize,
    reasoning_bytes: usize,
}

impl Held {
    fn push(&mut self, field: Field, character: char, room: usize) -> Result<(), BoundaryError> {
        if self.text.len().saturating_add(character.len_utf8()) > room {
            return Err(BoundaryError::BufferOverflow);
        }
        self.text.push(character);
        if field == Field::Reasoning {
            self.reasoning_bytes = self.text.len();
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
enum Mode {
    #[default]
    Plain,
    Run(char, usize, bool),
    Inline(usize, usize),
    Head(char, usize),
    Fence(char, usize),
    Indented,
}

enum Step {
    Hold,
    Plain,
    Protected,
    Flush(Held),
    FlushRetry(Held),
    Done,
    Replay(Held),
}

#[derive(Default)]
struct Markdown {
    mode: Mode,
    held: Held,
    line_indent: u8,
    escaped: bool,
}

impl Markdown {
    fn push(
        &mut self,
        field: Field,
        character: char,
        pending_bytes: usize,
        limit: usize,
    ) -> Result<Step, BoundaryError> {
        use Mode::*;
        use Step::{Flush, FlushRetry, Hold, Protected};
        let room = limit.saturating_sub(pending_bytes);
        loop {
            match self.mode {
                Plain => {
                    if self.escaped {
                        self.escaped = false;
                        self.observe_plain(character);
                        return Ok(Protected);
                    }
                    let line_start = self.line_indent <= 3;
                    if (character == '\t' && line_start)
                        || (character == ' ' && self.line_indent == 3)
                    {
                        self.mode = Indented;
                        return Ok(Protected);
                    }
                    if character == '`' || (character == '~' && line_start) {
                        self.held.push(field, character, room)?;
                        self.held.opener_bytes = self.held.text.len();
                        self.line_indent = u8::MAX;
                        self.mode = Run(character, 1, line_start);
                        return Ok(Hold);
                    }
                    self.escaped = character == '\\';
                    self.observe_plain(character);
                    return Ok(Step::Plain);
                }
                Run(symbol, width, line_start) if character == symbol => {
                    self.held.push(field, character, room)?;
                    self.held.opener_bytes = self.held.text.len();
                    self.mode = Run(symbol, width + 1, line_start);
                    return Ok(Hold);
                }
                Run('`', width, line_start) => {
                    self.mode = if line_start && width >= 3 {
                        Head('`', width)
                    } else {
                        Inline(width, 0)
                    };
                }
                Run(symbol, width, _) if width >= 3 => {
                    self.mode = Head(symbol, width);
                }
                Run(..) => {
                    self.mode = Plain;
                    self.line_indent = u8::MAX;
                    return Ok(FlushRetry(std::mem::take(&mut self.held)));
                }
                Inline(width, run) if character == '`' => {
                    self.held.push(field, character, room)?;
                    self.mode = Inline(width, run + 1);
                    return Ok(Hold);
                }
                Inline(width, run) if run == width => {
                    self.mode = Plain;
                    self.line_indent = u8::MAX;
                    return Ok(FlushRetry(std::mem::take(&mut self.held)));
                }
                Inline(width, _) => {
                    self.held.push(field, character, room)?;
                    self.mode = Inline(width, 0);
                    return Ok(Hold);
                }
                Head('`', width) if character == '`' => {
                    self.mode = Inline(width, 0);
                }
                Head(symbol, width) => {
                    self.held.push(field, character, room)?;
                    if character == '\n' {
                        self.mode = Fence(symbol, width);
                        return Ok(Flush(std::mem::take(&mut self.held)));
                    }
                    return Ok(Hold);
                }
                Fence(symbol, width) => {
                    self.held.push(field, character, room)?;
                    if character != '\n' {
                        return Ok(Hold);
                    }
                    self.mode = if fence_closes(&self.held.text, symbol, width) {
                        self.line_indent = 0;
                        Plain
                    } else {
                        Fence(symbol, width)
                    };
                    return Ok(Flush(std::mem::take(&mut self.held)));
                }
                Indented => {
                    if character == '\n' {
                        self.mode = Plain;
                        self.line_indent = 0;
                    }
                    return Ok(Protected);
                }
            }
        }
    }

    fn finish(&mut self) -> Step {
        use Mode::*;
        let result = match self.mode {
            Plain | Indented => Step::Done,
            Inline(width, run) if run != width => {
                self.line_indent = u8::MAX;
                Step::Replay(std::mem::take(&mut self.held))
            }
            _ => Step::Flush(std::mem::take(&mut self.held)),
        };
        self.mode = Plain;
        result
    }

    fn promote_reasoning(&mut self) {
        if !matches!(self.mode, Mode::Plain) {
            self.held.reasoning_bytes = self.held.text.len();
        }
    }

    fn observe_plain(&mut self, character: char) {
        if character == '\n' {
            self.line_indent = 0;
        } else if character == ' ' {
            self.line_indent = self.line_indent.saturating_add(1);
        } else {
            self.line_indent = u8::MAX;
        }
    }
}

fn fence_closes(line: &str, symbol: char, width: usize) -> bool {
    let line = line.trim_end_matches(['\r', '\n']);
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    let rest = &line[indent..];
    let run = rest
        .chars()
        .take_while(|character| *character == symbol)
        .count();
    indent <= 3
        && run >= width
        && rest[run..]
            .chars()
            .all(|character| matches!(character, ' ' | '\t'))
}

fn is_semantic_neighbor(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '<' | '`')
}

fn is_structural_marker(opens: bool, left: Option<char>, right: Option<char>) -> bool {
    let attached_left = left.is_some_and(is_semantic_neighbor);
    let attached_right = right.is_some_and(is_semantic_neighbor);
    let line_left = left.is_none_or(|character| matches!(character, '\r' | '\n'));
    let line_right = right.is_none_or(|character| matches!(character, '\r' | '\n'));
    // A bare close marker attached at line end is protocol; literal examples
    // must be whitespace-delimited or protected as Markdown.
    let attached_before_line_end =
        !opens && line_right && left.is_some_and(|character| !character.is_whitespace());
    (opens && line_left)
        || attached_left
        || attached_right
        || attached_before_line_end
        || (line_left && line_right)
}

fn is_literal_enclosure(left: Option<char>, right: Option<char>) -> bool {
    matches!(
        (left, right),
        (Some('('), Some(')'))
            | (Some('['), Some(']'))
            | (Some('{'), Some('}'))
            | (Some('"'), Some('"'))
            | (Some('\''), Some('\''))
    )
}

/// Repairs the GLM hybrid-reasoning boundary without guessing from prose.
///
/// Before the first unprotected close marker, content is provisional: a later
/// close reclassifies it as reasoning, while a normal tool/terminal boundary
/// commits it as content. This buffering is required because already-streamed
/// content cannot be moved to a reasoning channel retroactively.
pub(crate) struct ReasoningBoundary {
    phase: Phase,
    markdown: Markdown,
    candidate: String,
    candidate_reasoning_bytes: usize,
    plain_tail: Option<char>,
    provisional: String,
    explicit_open: bool,
    authoritative_reasoning: bool,
    // Marker lookahead is separately bounded by the longest static marker.
    max_provisional_bytes: usize,
}

impl ReasoningBoundary {
    pub(crate) fn new(max_provisional_bytes: usize) -> Self {
        Self {
            phase: Phase::Unresolved,
            markdown: Markdown::default(),
            candidate: String::new(),
            candidate_reasoning_bytes: 0,
            plain_tail: None,
            provisional: String::new(),
            explicit_open: false,
            authoritative_reasoning: false,
            max_provisional_bytes,
        }
    }

    pub(crate) fn push(
        &mut self,
        field: Field,
        text: &str,
    ) -> Result<BoundaryOutput, BoundaryError> {
        if self.phase == Phase::Done {
            return Err(BoundaryError::Malformed);
        }

        let mut output = BoundaryOutput::default();
        if self.phase == Phase::Unresolved && field == Field::Reasoning && !text.is_empty() {
            self.authoritative_reasoning = true;
            output
                .reasoning
                .push_str(&std::mem::take(&mut self.provisional));
            self.candidate_reasoning_bytes = self.candidate.len();
            self.markdown.promote_reasoning();
        }

        self.scan(field, text, &mut output)?;
        Ok(output)
    }

    pub(crate) fn finish(&mut self) -> Result<BoundaryOutput, BoundaryError> {
        self.settle(true)
    }

    pub(crate) fn commit_tool_boundary(
        &mut self,
        promotes_reasoning: bool,
    ) -> Result<BoundaryOutput, BoundaryError> {
        let promote =
            promotes_reasoning && self.phase == Phase::Unresolved && !self.authoritative_reasoning;
        let mut output = self.settle(false)?;
        if promote && self.phase == Phase::Unresolved {
            output.reasoning.push_str(&output.content);
            output.content.clear();
        }
        Ok(output)
    }

    fn settle(&mut self, terminal: bool) -> Result<BoundaryOutput, BoundaryError> {
        match self.phase {
            Phase::Done if terminal => return Ok(BoundaryOutput::default()),
            Phase::Done => return Err(BoundaryError::Malformed),
            _ => {}
        }
        let mut output = BoundaryOutput::default();
        self.commit_into(&mut output)?;
        if terminal {
            self.phase = Phase::Done;
        }
        Ok(output)
    }

    pub(crate) fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    pub(crate) fn pending_bytes(&self) -> usize {
        self.provisional
            .len()
            .saturating_add(self.candidate.len())
            .saturating_add(self.markdown.held.text.len())
    }

    fn commit_into(&mut self, output: &mut BoundaryOutput) -> Result<(), BoundaryError> {
        loop {
            match self.markdown.finish() {
                Step::Done => break,
                Step::Flush(held) => {
                    self.consume(&held.text, held.reasoning_bytes, false, output)?
                }
                Step::Replay(held) => {
                    let at = held.opener_bytes;
                    self.consume(
                        &held.text[..at],
                        held.reasoning_bytes.min(at),
                        false,
                        output,
                    )?;
                    self.consume(
                        &held.text[at..],
                        held.reasoning_bytes.saturating_sub(at),
                        true,
                        output,
                    )?;
                }
                _ => unreachable!("non-terminal Markdown step"),
            }
        }
        if !self.candidate.is_empty() {
            if let Some(opens) = self.pending_marker() {
                if (!opens && self.phase == Phase::Unresolved && self.authoritative_reasoning)
                    || (self.explicit_open && !opens)
                    || is_structural_marker(opens, self.plain_tail, None)
                {
                    self.resolve_marker(opens, output)?;
                } else {
                    self.flush_candidate(output)?;
                }
            } else {
                self.flush_candidate(output)?;
            }
        }
        if self.explicit_open {
            return Err(BoundaryError::Malformed);
        }
        if self.phase == Phase::Unresolved {
            output
                .content
                .push_str(&std::mem::take(&mut self.provisional));
        }
        Ok(())
    }

    fn scan(
        &mut self,
        field: Field,
        text: &str,
        output: &mut BoundaryOutput,
    ) -> Result<(), BoundaryError> {
        for character in text.chars() {
            let mut current = Some(character);
            while let Some(character) = current.take() {
                let field = if self.phase == Phase::Content {
                    Field::Content
                } else {
                    field
                };
                if !self.candidate.is_empty() {
                    if let Some(opens) = self.pending_marker() {
                        let authoritative_close = !opens
                            && self.phase == Phase::Unresolved
                            && self.authoritative_reasoning
                            && !is_literal_enclosure(self.plain_tail, Some(character));
                        let structural = authoritative_close
                            || (self.explicit_open && !opens)
                            || is_structural_marker(opens, self.plain_tail, Some(character));
                        if structural {
                            self.resolve_marker(opens, output)?;
                        } else {
                            self.flush_candidate(output)?;
                        }
                        current = Some(character);
                        continue;
                    }
                    let mut extended = self.candidate.clone();
                    extended.push(character);
                    if let Some((_, opens)) = MARKERS.iter().find(|(tag, _)| *tag == extended) {
                        self.candidate = extended;
                        if field == Field::Reasoning {
                            self.candidate_reasoning_bytes = self.candidate.len();
                        }
                        if self.explicit_open
                            && *opens
                            && self.plain_tail.is_some_and(is_semantic_neighbor)
                        {
                            self.resolve_marker(*opens, output)?;
                        }
                    } else if MARKERS.iter().any(|(tag, _)| tag.starts_with(&extended)) {
                        self.candidate = extended;
                        if field == Field::Reasoning {
                            self.candidate_reasoning_bytes = self.candidate.len();
                        }
                    } else {
                        self.flush_candidate(output)?;
                        current = Some(character);
                    }
                    continue;
                }

                match self.markdown.push(
                    field,
                    character,
                    self.provisional.len(),
                    self.max_provisional_bytes,
                )? {
                    Step::Hold => {}
                    Step::Plain if character == '<' => {
                        self.candidate.push(character);
                        self.candidate_reasoning_bytes = usize::from(field == Field::Reasoning);
                    }
                    Step::Plain | Step::Protected => {
                        let mut encoded = [0; 4];
                        self.route(field, character.encode_utf8(&mut encoded), output)?;
                    }
                    Step::Flush(held) => {
                        self.consume(&held.text, held.reasoning_bytes, false, output)?
                    }
                    Step::FlushRetry(held) => {
                        self.consume(&held.text, held.reasoning_bytes, false, output)?;
                        current = Some(character);
                    }
                    Step::Done | Step::Replay(_) => unreachable!("terminal-only Markdown step"),
                }
            }
        }
        Ok(())
    }

    fn pending_marker(&self) -> Option<bool> {
        MARKERS
            .iter()
            .find_map(|(tag, opens)| (*tag == self.candidate).then_some(*opens))
    }

    fn resolve_marker(
        &mut self,
        opens: bool,
        output: &mut BoundaryOutput,
    ) -> Result<(), BoundaryError> {
        self.candidate.clear();
        self.candidate_reasoning_bytes = 0;
        self.plain_tail = None;
        self.markdown = Markdown::default();
        match opens {
            true if self.phase == Phase::Unresolved && !self.explicit_open => {
                self.explicit_open = true;
            }
            false if self.phase == Phase::Unresolved => {
                output
                    .reasoning
                    .push_str(&std::mem::take(&mut self.provisional));
                self.explicit_open = false;
                self.phase = Phase::Content;
            }
            _ => return Err(BoundaryError::Malformed),
        }
        Ok(())
    }

    fn flush_candidate(&mut self, output: &mut BoundaryOutput) -> Result<(), BoundaryError> {
        let candidate = std::mem::take(&mut self.candidate);
        let split = std::mem::take(&mut self.candidate_reasoning_bytes);
        self.consume(&candidate, split, false, output)
    }

    fn consume(
        &mut self,
        text: &str,
        reasoning_bytes: usize,
        rescan: bool,
        output: &mut BoundaryOutput,
    ) -> Result<(), BoundaryError> {
        for (field, text) in [
            (Field::Reasoning, &text[..reasoning_bytes]),
            (Field::Content, &text[reasoning_bytes..]),
        ] {
            if !text.is_empty() {
                if rescan {
                    self.scan(field, text, output)?;
                } else {
                    self.route(field, text, output)?;
                }
            }
        }
        Ok(())
    }

    fn route(
        &mut self,
        field: Field,
        text: &str,
        output: &mut BoundaryOutput,
    ) -> Result<(), BoundaryError> {
        match self.phase {
            Phase::Unresolved if field == Field::Reasoning => output.reasoning.push_str(text),
            Phase::Unresolved => {
                if self.provisional.len().saturating_add(text.len()) > self.max_provisional_bytes {
                    return Err(BoundaryError::BufferOverflow);
                }
                self.provisional.push_str(text);
            }
            Phase::Content => output.content.push_str(text),
            _ => return Err(BoundaryError::Malformed),
        }
        if let Some(character) = text.chars().next_back() {
            self.plain_tail = Some(character);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(parts: &[(Field, &str)], finish: bool) -> Result<BoundaryOutput, BoundaryError> {
        let mut boundary = ReasoningBoundary::new(4096);
        let mut output = BoundaryOutput::default();
        for (field, text) in parts {
            output.append(boundary.push(*field, text)?);
        }
        if finish {
            output.append(boundary.finish()?);
        }
        Ok(output)
    }

    fn expected(reasoning: &str, content: &str) -> BoundaryOutput {
        BoundaryOutput {
            reasoning: reasoning.into(),
            content: content.into(),
        }
    }

    fn points(text: &str) -> Vec<usize> {
        text.char_indices()
            .map(|(index, _)| index)
            .chain([text.len()])
            .collect()
    }

    fn assert_splits(text: &str, reasoning: &str, content: &str, field_pairs: &[(Field, Field)]) {
        for split in points(text) {
            for &(first, second) in field_pairs {
                assert_eq!(
                    run(&[(first, &text[..split]), (second, &text[split..])], true),
                    Ok(expected(reasoning, content)),
                    "split {split}, fields {first:?}/{second:?}"
                );
            }
        }
    }

    fn assert_boundary(text: &str, reasoning: &str, content: &str) {
        assert_splits(
            text,
            reasoning,
            content,
            &[
                (Field::Content, Field::Content),
                (Field::Reasoning, Field::Content),
                (Field::Content, Field::Reasoning),
            ],
        );
    }

    fn assert_content(text: &str) {
        assert_splits(text, "", text, &[(Field::Content, Field::Content)]);
    }

    fn assert_run(parts: &[(Field, &str)], reasoning: &str, content: &str) {
        assert_eq!(run(parts, true), Ok(expected(reasoning, content)));
    }

    #[test]
    fn literal_and_structural_marker_matrices_are_partition_safe() {
        for text in [
            "The model leaked reasoning delimiters (open or /</think>) into visible output",
            "- </think>: a literal close marker",
            r#"Call it "</think>" here."#,
        ] {
            assert_content(text);
        }
        assert_content("Use <think> then continue.");
        for (text, reasoning, content) in [
            ("<think>plan</think>answer", "plan", "answer"),
            ("<think> plan</think>answer", " plan", "answer"),
            ("<thinking>\nplan</thinking>answer", "\nplan", "answer"),
            ("plan</think>answer", "plan", "answer"),
            ("plan</think> answer", "plan", " answer"),
            ("(</think>)plan</think>answer", "(</think>)plan", "answer"),
            ("plan</think>", "plan", ""),
            ("Xong…</thinking>\nĐáp án", "Xong…", "\nĐáp án"),
        ] {
            assert_boundary(text, reasoning, content);
        }
    }

    #[test]
    fn observed_field_transition_matrix_is_preserved() {
        for (parts, reasoning, content) in [
            (
                vec![
                    (Field::Reasoning, "Plan."),
                    (Field::Content, " Continue the plan</thi"),
                    (Field::Content, "nk>Final answer"),
                ],
                "Plan. Continue the plan",
                "Final answer",
            ),
            (
                vec![
                    (Field::Reasoning, "Done."),
                    (Field::Content, " Use `</think>` literally."),
                ],
                "Done.",
                " Use `</think>` literally.",
            ),
            (
                vec![
                    (Field::Content, "19/19 tests pass. Now review."),
                    (Field::Content, "</think>19/19 tests pass."),
                ],
                "19/19 tests pass. Now review.",
                "19/19 tests pass.",
            ),
            (
                vec![
                    (Field::Reasoning, "plan</think>answer<"),
                    (Field::Content, "not-a-tag"),
                ],
                "plan",
                "answer<not-a-tag",
            ),
        ] {
            assert_run(&parts, reasoning, content);
        }

        assert_run(
            &[(Field::Reasoning, "</"), (Field::Content, "thiX")],
            "</",
            "thiX",
        );

        let mut boundary = ReasoningBoundary::new(4096);
        assert_eq!(
            boundary.push(Field::Reasoning, "plan</think>answer"),
            Ok(expected("plan", "answer"))
        );
        assert_eq!(
            boundary.push(Field::Reasoning, " later").unwrap().content,
            " later"
        );
    }

    #[test]
    fn markdown_protection_is_chunk_field_and_utf8_safe() {
        assert_boundary(
            "Inspect `</think>` then continue</think>answer",
            "Inspect `</think>` then continue",
            "answer",
        );
        assert_boundary(
            "Inspect `typo </thinking>answer",
            "Inspect `typo ",
            "answer",
        );

        for text in [
            "\x60\x60\x60text\n</think>\n\x60\x60\x60\nanswer",
            "~~~text\n</thinking>\n~~~\nanswer",
            "    </think>\nanswer",
            "\\</think> literal",
        ] {
            assert_run(&[(Field::Content, text)], "", text);
        }

        assert_boundary(
            "reason</think>  ```lang\nliteral </think>\n",
            "reason",
            "  ```lang\nliteral </think>\n",
        );
        assert_eq!(
            run(
                &[(
                    Field::Content,
                    "<think>\x60\x60\x60lang\nliteral </think>\n"
                )],
                true,
            ),
            Err(BoundaryError::Malformed)
        );

        for (text, reasoning, content) in [
            (
                "\x60\x60\x60text\r\nliteral </think>\r\n\x60\x60\x60\r\nplan</think>answer",
                "\x60\x60\x60text\r\nliteral </think>\r\n\x60\x60\x60\r\nplan",
                "answer",
            ),
            (
                "lý do \x60</thinking>\x60 tiếp tục</thinking>đáp án",
                "lý do \x60</thinking>\x60 tiếp tục",
                "đáp án",
            ),
        ] {
            assert_boundary(text, reasoning, content);
        }

        assert_content("\\\\\\</thinking>");
        assert_boundary("\\\\</think>answer", "\\\\", "answer");
    }

    #[test]
    fn bounded_buffer_and_terminal_lifecycle_fail_closed() {
        let literal = "\x60</think>\x60";
        let mut exact = ReasoningBoundary::new(literal.len());
        assert_eq!(
            exact.push(Field::Content, literal).unwrap(),
            BoundaryOutput::default()
        );
        assert_eq!(exact.finish().unwrap().content, literal);
        assert_eq!(
            ReasoningBoundary::new(literal.len() - 1).push(Field::Content, literal),
            Err(BoundaryError::BufferOverflow)
        );

        let mut second_close = ReasoningBoundary::new(4096);
        assert!(second_close
            .push(Field::Content, "plan</think>answer")
            .is_ok());
        assert_eq!(
            second_close.push(Field::Content, "</think>garbage"),
            Err(BoundaryError::Malformed)
        );

        let mut tool = ReasoningBoundary::new(4096);
        tool.push(Field::Content, "private reasoning").unwrap();
        assert_eq!(
            tool.commit_tool_boundary(true),
            Ok(expected("private reasoning", ""))
        );

        let mut ordinary_tool = ReasoningBoundary::new(4096);
        ordinary_tool
            .push(Field::Content, "visible preamble")
            .unwrap();
        assert_eq!(
            ordinary_tool.commit_tool_boundary(false),
            Ok(expected("", "visible preamble"))
        );

        let mut exact_limit = ReasoningBoundary::new(3);
        assert_eq!(
            exact_limit.push(Field::Content, "abc").unwrap(),
            BoundaryOutput::default()
        );
        assert_eq!(exact_limit.finish().unwrap().content, "abc");
        assert_eq!(
            ReasoningBoundary::new(3).push(Field::Content, "abcd"),
            Err(BoundaryError::BufferOverflow)
        );
        assert_eq!(exact_limit.finish(), Ok(BoundaryOutput::default()));
    }

    #[test]
    fn malformed_explicit_blocks_are_rejected() {
        for text in ["<think>plan", "<think>A<think>B</think>answer"] {
            let mut boundary = ReasoningBoundary::new(4096);
            let result = boundary.push(Field::Content, text);
            if text == "<think>plan" {
                assert!(result.is_ok());
                assert_eq!(boundary.finish(), Err(BoundaryError::Malformed));
            } else {
                assert_eq!(result, Err(BoundaryError::Malformed));
            }
        }
    }
}
