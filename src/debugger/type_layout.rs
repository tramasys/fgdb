//! Bounded presentation data from the compiler-described layout reader.

pub(crate) const LAYOUT_UNITS: &str =
    "Offsets relative to each type · Bytes unless marked b · ? unavailable";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TypeLayout {
    pub address: Option<u64>,
    pub bytes: usize,
    pub rows: Vec<LayoutRow>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LayoutRow {
    Type {
        name: String,
        size: String,
        alignment: Option<String>,
    },
    Field([String; 4]),
    Note(String),
}

impl TypeLayout {
    pub(crate) fn parse(text: &str, address: Option<u64>, bytes: usize) -> Option<Self> {
        if text.len() > 66 * 1024 {
            return None;
        }

        let mut rows = Vec::new();
        let mut types = 0;

        for line in text.lines() {
            if rows.len() >= 257 || line.contains('\0') {
                return None;
            }

            let fields: Vec<_> = line.splitn(6, '\t').collect();

            let row = match fields.as_slice() {
                ["type", name, size, alignment] if rows.is_empty() => {
                    types += 1;

                    LayoutRow::Type {
                        name: (*name).into(),
                        size: (*size).into(),
                        alignment: Some((*alignment).into()),
                    }
                }
                ["pointee", name, size] if types == 1 => {
                    types += 1;

                    LayoutRow::Type {
                        name: (*name).into(),
                        size: (*size).into(),
                        alignment: None,
                    }
                }
                ["field", offset, size, name, type_name] if types > 0 => {
                    LayoutRow::Field([offset, size, name, type_name].map(|value| (*value).into()))
                }
                ["note", note] if types > 0 => LayoutRow::Note((*note).into()),
                _ => return None,
            };

            rows.push(row);
        }

        (types > 0).then_some(Self {
            address,
            bytes,
            rows,
        })
    }

    pub(crate) fn text(&self) -> String {
        self.rows
            .iter()
            .map(|row| match row {
                LayoutRow::Type {
                    name,
                    size,
                    alignment,
                } => match alignment {
                    Some(alignment) => format!(
                        "Type  {name}\nSize  {size} bytes\nAlignment  {alignment} bytes\nOFFSET\tSIZE\tFIELD\tTYPE"
                    ),
                    None => format!(
                        "\nPointee type  {name}\nSize  {size} bytes\nOFFSET\tSIZE\tFIELD\tTYPE"
                    ),
                },
                LayoutRow::Field(cells) => cells.join("\t"),
                LayoutRow::Note(note) => note.clone(),
            })
            .chain(std::iter::once(LAYOUT_UNITS.into()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
