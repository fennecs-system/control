//! Lifecycle states saved as text. Each type lists its exact saved strings
//! once; saved text no current code writes loads as `Unrecognised` and saves
//! back unchanged, so old or hand-edited state never fails to load.
macro_rules! status_enum {
    ($(#[$meta:meta])* $vis:vis enum $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        $vis enum $name {
            $($(#[$vmeta])* $variant,)+
            /// Saved text no current code writes, kept verbatim.
            Unrecognised(String),
        }
        impl $name {
            /// Every value current code knows, in declaration order.
            #[cfg(test)]
            pub(crate) const KNOWN: &[Self] = &[$(Self::$variant,)+];
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $text,)+
                    Self::Unrecognised(text) => text,
                }
            }
            pub fn parse(text: &str) -> Self {
                match text {
                    $($text => Self::$variant,)+
                    other => Self::Unrecognised(other.to_owned()),
                }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                <String as serde::Deserialize>::deserialize(deserializer).map(|text| Self::parse(&text))
            }
        }
    };
}
pub(crate) use status_enum;

