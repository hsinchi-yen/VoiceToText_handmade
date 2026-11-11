//! IBusSerializable variants, as built by ibus 1.5.26 `src/ibusserializable.c`:
//! every object is a struct `(s a{sv} ...)` = (GType name, attachments, own fields).
//!
//! - `IBusText`: `(sa{sv}sv)` — text, then `v` holding an `IBusAttrList` `(sa{sv}av)`.
//! - `IBusEngineDesc`: `(sa{sv}` + 8×`s` + `u` + 8×`s` + `)` — name, longname, description,
//!   language, license, author, icon, layout, rank (u), hotkeys, symbol, setup,
//!   layout_variant, layout_option, version, textdomain, icon_prop_key.
//! - `IBusComponent`: `(sa{sv}ssssssssavav)` — name, description, version, license,
//!   author, homepage, exec, textdomain, observed paths (`av`), engines (`av`).
//!
//! Text is only assembled in memory for the D-Bus message; it is never logged.

use std::collections::HashMap;

use zbus::zvariant::{StructureBuilder, Value};

fn empty_attachments() -> HashMap<String, Value<'static>> {
    HashMap::new()
}

/// `IBusText` with no attributes, ready to be sent as the `v` argument of `CommitText`.
pub(crate) fn ibus_text(text: &str) -> Value<'_> {
    let attrs = StructureBuilder::new()
        .add_field("IBusAttrList")
        .add_field(empty_attachments())
        .add_field(Vec::<Value<'static>>::new())
        .build()
        .expect("IBusAttrList fields are non-empty");
    Value::Structure(
        StructureBuilder::new()
            .add_field("IBusText")
            .add_field(empty_attachments())
            .add_field(text)
            .append_field(Value::Value(Box::new(Value::Structure(attrs))))
            .build()
            .expect("IBusText fields are non-empty"),
    )
}

/// Reads the text back out of a serialized `IBusText` (accepts the struct or a variant
/// wrapping it). `None` if the value is not an `IBusText`.
#[cfg(test)]
pub(crate) fn text_of(value: &Value<'_>) -> Option<String> {
    match value {
        Value::Value(inner) => text_of(inner),
        Value::Structure(s) => match s.fields() {
            [Value::Str(ty), _, Value::Str(text), _] if ty.as_str() == "IBusText" => {
                Some(text.as_str().to_string())
            }
            _ => None,
        },
        _ => None,
    }
}

/// One engine of a component (`IBusEngineDesc`). Fields left empty are sent as "".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EngineDesc {
    /// Unique engine name; this is what `SetEngine` / GNOME input sources refer to.
    pub name: String,
    pub longname: String,
    pub description: String,
    /// e.g. `zh_TW`.
    pub language: String,
    pub license: String,
    pub author: String,
    pub icon: String,
    /// XKB layout used while the engine is active, e.g. `us`.
    pub layout: String,
    pub rank: u32,
    pub symbol: String,
    pub version: String,
}

impl EngineDesc {
    pub(crate) fn to_value(&self) -> Value<'static> {
        let strings_before_rank = [
            &self.name,
            &self.longname,
            &self.description,
            &self.language,
            &self.license,
            &self.author,
            &self.icon,
            &self.layout,
        ];
        let empty = String::new();
        // hotkeys, symbol, setup, layout_variant, layout_option, version, textdomain,
        // icon_prop_key
        let strings_after_rank = [
            &empty,
            &self.symbol,
            &empty,
            &empty,
            &empty,
            &self.version,
            &empty,
            &empty,
        ];
        let mut b = StructureBuilder::new()
            .add_field("IBusEngineDesc")
            .add_field(empty_attachments());
        for s in strings_before_rank {
            b = b.add_field(s.clone());
        }
        b = b.add_field(self.rank);
        for s in strings_after_rank {
            b = b.add_field(s.clone());
        }
        Value::Structure(b.build().expect("IBusEngineDesc fields are non-empty"))
    }
}

/// An IBus component (`IBusComponent`) for `RegisterComponent`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComponentDesc {
    /// The component's D-Bus name, e.g. `org.freedesktop.IBus.V2c`.
    pub name: String,
    pub description: String,
    pub version: String,
    pub license: String,
    pub author: String,
    pub homepage: String,
    /// Command line ibus-daemon runs to start the component; empty when registered by a
    /// running process.
    pub exec: String,
    pub textdomain: String,
    pub engines: Vec<EngineDesc>,
}

impl ComponentDesc {
    pub(crate) fn to_value(&self) -> Value<'static> {
        let engines: Vec<Value<'static>> = self.engines.iter().map(EngineDesc::to_value).collect();
        let mut b = StructureBuilder::new()
            .add_field("IBusComponent")
            .add_field(empty_attachments());
        for s in [
            &self.name,
            &self.description,
            &self.version,
            &self.license,
            &self.author,
            &self.homepage,
            &self.exec,
            &self.textdomain,
        ] {
            b = b.add_field(s.clone());
        }
        let b = b
            .add_field(Vec::<Value<'static>>::new()) // observed paths
            .add_field(engines);
        Value::Structure(b.build().expect("IBusComponent fields are non-empty"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{serialized::Context, to_bytes, LE};

    fn sig(v: &Value<'_>) -> String {
        v.value_signature().to_string()
    }

    #[test]
    fn ibus_text_signature() {
        assert_eq!(sig(&ibus_text("x")), "(sa{sv}sv)");
        let Value::Structure(s) = ibus_text("x") else {
            panic!()
        };
        let Value::Value(attrs) = &s.fields()[3] else {
            panic!()
        };
        assert_eq!(sig(attrs), "(sa{sv}av)");
    }

    /// Hand-encoded D-Bus marshalling (little endian) of `IBusText("a")`, compared byte by
    /// byte with what goes on the wire.
    #[test]
    fn ibus_text_bytes() {
        fn pad(b: &mut Vec<u8>, n: usize) {
            while !b.len().is_multiple_of(n) {
                b.push(0);
            }
        }
        fn string(b: &mut Vec<u8>, s: &str) {
            pad(b, 4);
            b.extend((s.len() as u32).to_le_bytes());
            b.extend(s.as_bytes());
            b.push(0);
        }
        fn empty_dict(b: &mut Vec<u8>) {
            pad(b, 4);
            b.extend(0u32.to_le_bytes());
            pad(b, 8); // dict entries are 8-aligned, even in an empty array
        }
        let mut want = Vec::new();
        string(&mut want, "IBusText");
        empty_dict(&mut want);
        string(&mut want, "a");
        // variant: signature, then the struct (8-aligned)
        let inner_sig = "(sa{sv}av)";
        want.push(inner_sig.len() as u8);
        want.extend(inner_sig.as_bytes());
        want.push(0);
        pad(&mut want, 8);
        string(&mut want, "IBusAttrList");
        empty_dict(&mut want);
        pad(&mut want, 4);
        want.extend(0u32.to_le_bytes()); // av, empty

        let ctxt = Context::new_dbus(LE, 0);
        let Value::Structure(s) = ibus_text("a") else {
            panic!()
        };
        let got = to_bytes(ctxt, &s).unwrap();
        assert_eq!(got.bytes(), &want[..]);
        assert_eq!(want.len(), 76);
    }

    #[test]
    fn text_round_trips_including_cjk_and_emoji() {
        for t in ["", "測試文字", "mixed 中英 text 🙂"] {
            let v = ibus_text(t);
            assert_eq!(text_of(&v).as_deref(), Some(t));
            let wrapped = Value::Value(Box::new(v));
            assert_eq!(text_of(&wrapped).as_deref(), Some(t));
        }
        assert_eq!(text_of(&Value::from(3u32)), None);
    }

    #[test]
    fn component_and_engine_signatures() {
        let c = ComponentDesc {
            name: "org.freedesktop.IBus.V2cTest".into(),
            engines: vec![EngineDesc {
                name: "v2c-test".into(),
                rank: 0,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(sig(&c.to_value()), "(sa{sv}ssssssssavav)");
        let eight = "s".repeat(8);
        assert_eq!(
            sig(&c.engines[0].to_value()),
            format!("(sa{{sv}}{eight}u{eight})")
        );
        let Value::Structure(s) = c.to_value() else {
            panic!()
        };
        assert_eq!(s.fields().len(), 12);
        let Value::Array(engines) = &s.fields()[11] else {
            panic!()
        };
        assert_eq!(engines.len(), 1);
        let e = match &engines.inner()[0] {
            Value::Value(e) => e.as_ref(),
            e => e,
        };
        let Value::Structure(e) = e else { panic!() };
        assert_eq!(e.fields().len(), 19);
        assert_eq!(e.fields()[0], Value::from("IBusEngineDesc"));
        assert_eq!(e.fields()[2], Value::from("v2c-test"));
        assert_eq!(e.fields()[10], Value::from(0u32));
    }
}
