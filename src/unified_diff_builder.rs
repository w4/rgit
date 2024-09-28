//! Heavily based on [`gix::diff::blob::UnifiedDiffBuilder`] but provides
//! a callback that can be used for styling the diffs.

use std::{fmt::Write, ops::Range};

use gix::diff::blob::{
    intern::{InternedInput, Interner, Token},
    Sink,
};

pub(crate) trait Callback {
    fn addition(&mut self, data: &str, dst: &mut String);
    fn remove(&mut self, data: &str, dst: &mut String);
    fn context(&mut self, data: &str, dst: &mut String);
}

impl<C: Callback> Callback for &mut C {
    fn addition(&mut self, data: &str, dst: &mut String) {
        (*self).addition(data, dst);
    }

    fn remove(&mut self, data: &str, dst: &mut String) {
        (*self).remove(data, dst);
    }

    fn context(&mut self, data: &str, dst: &mut String) {
        (*self).context(data, dst);
    }
}

/// A [`Sink`] that creates a textual diff
/// in the format typically output by git or gnu-diff if the `-u` option is used
pub struct UnifiedDiffBuilder<'a, C, W>
where
    C: Callback,
    W: Write,
{
    before: &'a [Token],
    after: &'a [Token],
    interner: &'a Interner<&'a str>,

    pos: u32,
    before_hunk_start: u32,
    after_hunk_start: u32,
    before_hunk_len: u32,
    after_hunk_len: u32,

    callback: C,
    buffer: String,
    dst: W,
}

impl<'a, C, W> UnifiedDiffBuilder<'a, C, W>
where
    C: Callback,
    W: Write,
{
    /// Create a new `UnifiedDiffBuilder` for the given `input`,
    /// that will writes it output to the provided implementation of [`Write`].
    pub fn with_writer(input: &'a InternedInput<&'a str>, writer: W, callback: C) -> Self {
        Self {
            before_hunk_start: 0,
            after_hunk_start: 0,
            before_hunk_len: 0,
            after_hunk_len: 0,
            buffer: String::with_capacity(8),
            dst: writer,
            interner: &input.interner,
            before: &input.before,
            after: &input.after,
            callback,
            pos: 0,
        }
    }

    fn flush(&mut self) {
        if self.before_hunk_len == 0 && self.after_hunk_len == 0 {
            return;
        }

        let end = (self.pos + 3).min(u32::try_from(self.before.len()).unwrap_or(u32::MAX));
        self.update_pos(end, end);

        writeln!(
            &mut self.dst,
            "@@ -{},{} +{},{} @@",
            self.before_hunk_start + 1,
            self.before_hunk_len,
            self.after_hunk_start + 1,
            self.after_hunk_len,
        )
        .unwrap();
        write!(&mut self.dst, "{}", &self.buffer).unwrap();
        self.buffer.clear();
        self.before_hunk_len = 0;
        self.after_hunk_len = 0;
    }

    fn update_pos(&mut self, print_to: u32, move_to: u32) {
        for token in &self.before[self.pos as usize..print_to as usize] {
            self.callback
                .context(self.interner[*token], &mut self.buffer);
        }
        let len = print_to - self.pos;
        self.pos = move_to;
        self.before_hunk_len += len;
        self.after_hunk_len += len;
    }
}

impl<C, W> Sink for UnifiedDiffBuilder<'_, C, W>
where
    C: Callback,
    W: Write,
{
    type Out = W;

    fn process_change(&mut self, before: Range<u32>, after: Range<u32>) {
        if before.start - self.pos > 6 {
            self.flush();
            self.pos = before.start - 3;
            self.before_hunk_start = self.pos;
            self.after_hunk_start = after.start - 3;
        }
        self.update_pos(before.start, before.end);
        self.before_hunk_len += before.end - before.start;
        self.after_hunk_len += after.end - after.start;

        for token in &self.before[before.start as usize..before.end as usize] {
            self.callback
                .remove(self.interner[*token], &mut self.buffer);
        }

        for token in &self.after[after.start as usize..after.end as usize] {
            self.callback
                .addition(self.interner[*token], &mut self.buffer);
        }
    }

    fn finish(mut self) -> Self::Out {
        self.flush();
        self.dst
    }
}
