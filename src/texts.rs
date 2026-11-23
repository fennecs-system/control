//! Many short strings in one allocation. Views copied out of state under the
//! lock (`crate::Read`) keep their ids and titles here, so the copy costs one
//! buffer rather than an allocation per string.
#[derive(Default)]
pub struct Texts {
    text: String,
    ends: Vec<usize>,
}
impl Texts {
    pub fn with_capacity(parts: usize, bytes: usize) -> Self {
        Self {
            text: String::with_capacity(bytes),
            ends: Vec::with_capacity(parts),
        }
    }
    pub fn push(&mut self, part: &str) {
        self.text.push_str(part);
        self.ends.push(self.text.len());
    }
    pub fn get(&self, index: usize) -> &str {
        let start = index.checked_sub(1).map_or(0, |previous| self.ends[previous]);
        &self.text[start..self.ends[index]]
    }
    pub fn len(&self) -> usize {
        self.ends.len()
    }
}
impl<'a> FromIterator<&'a str> for Texts {
    fn from_iter<I: IntoIterator<Item = &'a str>>(parts: I) -> Self {
        let mut texts = Self::default();
        parts.into_iter().for_each(|part| texts.push(part));
        texts
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn texts_read_back_as_pushed() {
        let texts: super::Texts = ["a", "", "ccc", "λ"].into_iter().collect();
        assert_eq!(
            (0..texts.len()).map(|index| texts.get(index)).collect::<Vec<_>>(),
            ["a", "", "ccc", "λ"]
        );
        assert_eq!((texts.len(), texts.get(2)), (4, "ccc"));
    }
}
