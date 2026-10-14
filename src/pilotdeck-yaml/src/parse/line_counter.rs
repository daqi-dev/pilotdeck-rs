// Port of yaml@2.9.0 dist/parse/line-counter.js (ISC).
#[derive(Clone, Debug, Default)]
pub struct LineCounter {
    pub line_starts: Vec<usize>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinePos {
    pub line: usize,
    pub col: usize,
}
impl LineCounter {
    pub fn add_new_line(&mut self, offset: usize) {
        self.line_starts.push(offset);
    }
    pub fn line_pos(&self, offset: usize) -> LinePos {
        let mut low = 0;
        let mut high = self.line_starts.len();
        while low < high {
            let mid = (low + high) >> 1;
            if self.line_starts[mid] < offset {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        if self.line_starts.get(low) == Some(&offset) {
            LinePos {
                line: low + 1,
                col: 1,
            }
        } else if low == 0 {
            LinePos {
                line: 0,
                col: offset,
            }
        } else {
            LinePos {
                line: low,
                col: offset - self.line_starts[low - 1] + 1,
            }
        }
    }
}
