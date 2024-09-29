use std::{
    collections::HashMap,
    fmt::{Formatter, Write},
};

use serde::{
    de::{value::MapAccessDeserializer, Error, MapAccess, Visitor},
    Deserialize, Deserializer,
};

#[derive(Deserialize)]
pub struct Theme {
    palette: HashMap<String, String>,
    #[serde(flatten)]
    definitions: HashMap<String, PaletteReference>,
}

pub enum PaletteReference {
    Foreground(String),
    WithModifiers(PaletteReferenceWithModifiers),
}

impl<'de> Deserialize<'de> for PaletteReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PaletteReferenceVisitor;

        impl<'de> Visitor<'de> for PaletteReferenceVisitor {
            type Value = PaletteReference;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("palette reference")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                Ok(PaletteReference::Foreground(v.to_string()))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                PaletteReferenceWithModifiers::deserialize(MapAccessDeserializer::new(map))
                    .map(PaletteReference::WithModifiers)
            }
        }

        deserializer.deserialize_any(PaletteReferenceVisitor)
    }
}

#[derive(Deserialize)]
pub struct PaletteReferenceWithModifiers {
    bg: Option<String>,
    fg: Option<String>,
    #[serde(default)]
    modifiers: Vec<Modifiers>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifiers {
    Underlined,
    Bold,
    Italic,
    CrossedOut,
    Reversed,
    Dim,
}

impl Theme {
    fn get_color<'a>(&'a self, reference: &'a str) -> &'a str {
        if reference.starts_with('#') {
            reference
        } else {
            self.palette
                .get(reference)
                .unwrap_or_else(|| panic!("bad palette ref {reference}"))
        }
    }

    pub fn build_css(&self) -> String {
        let mut out = String::new();

        for (kind, palette_ref) in &self.definitions {
            write!(out, ".highlight.{kind} {{").unwrap();

            match palette_ref {
                PaletteReference::Foreground(color) => {
                    let color = self.get_color(color);
                    write!(out, "color:{color};").unwrap();
                }
                PaletteReference::WithModifiers(PaletteReferenceWithModifiers {
                    bg,
                    fg,
                    modifiers,
                }) => {
                    if let Some(color) = bg {
                        let color = self.get_color(color);
                        write!(out, "background:{color};").unwrap();
                    }

                    if let Some(color) = fg {
                        let color = self.get_color(color);
                        write!(out, "color:{color};").unwrap();
                    }

                    for modifier in modifiers {
                        match modifier {
                            Modifiers::Underlined => out.push_str("text-decoration:underline;"),
                            Modifiers::Bold => out.push_str("font-weight:bold;"),
                            Modifiers::Italic => out.push_str("font-style:italic;"),
                            Modifiers::CrossedOut => out.push_str("text-decoration:line-through;"),
                            Modifiers::Reversed | Modifiers::Dim => {}
                        }
                    }
                }
            }

            out.push('}');
        }

        out
    }
}
