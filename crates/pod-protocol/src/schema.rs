//! OpenAPI schema helpers for the tagged unions and open code sets on the
//! wire.
//!
//! utoipa only emits a `discriminator` for `#[serde(untagged)]` enums, while
//! the wire unions here are internally tagged (`type` on mutations, `status`
//! on podcast scopes, `deleted` on scope episodes). Code generators such as
//! the Android client's work best when each variant is its own component
//! that carries the tag property itself, so these helpers take a variant
//! struct's derived schema and add the tag as a single-value enum.

use serde::Serialize;
use utoipa::ToSchema;
use utoipa::openapi::schema::{Discriminator, Object, ObjectBuilder, OneOfBuilder, Schema, Type};
use utoipa::openapi::{Ref, RefOr};

/// One variant of a tagged union: its tag value and its payload type.
pub(crate) struct Variant {
    pub tag: serde_json::Value,
    pub name: String,
    schema: RefOr<Schema>,
    dependencies: fn(&mut Vec<(String, RefOr<Schema>)>),
}

impl Variant {
    pub fn of<T: ToSchema>(tag: impl Into<serde_json::Value>) -> Self {
        Self {
            tag: tag.into(),
            name: T::name().into_owned(),
            schema: T::schema(),
            dependencies: T::schemas,
        }
    }
}

/// `oneOf` over `variants` by reference, with a discriminator mapping when
/// the tag values are strings (OpenAPI only allows string discriminators).
pub(crate) fn one_of(tag_property: &str, variants: &[Variant]) -> RefOr<Schema> {
    let string_tags = variants.iter().all(|variant| variant.tag.is_string());
    let discriminator = string_tags.then(|| {
        Discriminator::with_mapping(
            tag_property,
            variants.iter().map(|variant| {
                (
                    variant.tag.as_str().expect("checked above").to_owned(),
                    format!("#/components/schemas/{}", variant.name),
                )
            }),
        )
    });
    variants
        .iter()
        .fold(OneOfBuilder::new(), |builder, variant| {
            builder.item(Ref::from_schema_name(&variant.name))
        })
        .discriminator(discriminator)
        .into()
}

/// Registers each variant's component, with the tag property added, plus the
/// components the variant references.
pub(crate) fn register(
    tag_property: &str,
    variants: Vec<Variant>,
    out: &mut Vec<(String, RefOr<Schema>)>,
) {
    for variant in variants {
        let RefOr::T(Schema::Object(object)) = variant.schema else {
            panic!("variant {} must derive an object schema", variant.name);
        };
        out.push((
            variant.name,
            with_tag(object, tag_property, variant.tag).into(),
        ));
        (variant.dependencies)(out);
    }
}

/// One variant's own schema with the tag property added, for a type that
/// is always that variant on the wire (a read endpoint's live episode).
pub(crate) fn tagged(tag_property: &str, variant: Variant) -> RefOr<Schema> {
    let RefOr::T(Schema::Object(object)) = variant.schema else {
        panic!("variant {} must derive an object schema", variant.name);
    };
    with_tag(object, tag_property, variant.tag).into()
}

fn with_tag(mut object: Object, property: &str, tag: serde_json::Value) -> Object {
    let schema_type = match &tag {
        serde_json::Value::Bool(_) => utoipa::openapi::Type::Boolean,
        _ => utoipa::openapi::Type::String,
    };
    let tag_schema = utoipa::openapi::ObjectBuilder::new()
        .schema_type(schema_type)
        .enum_values(Some([tag]))
        .build();
    object
        .properties
        .insert(property.to_owned(), tag_schema.into());
    object.required.insert(0, property.to_owned());
    object
}

/// The schema of an open code set: a plain string whose description lists
/// the known values. A JSON Schema `enum` would make code generators (the
/// Android client's Kotlin types) emit a closed enum that throws on a code
/// added later, which is exactly what the open set is meant to allow.
pub(crate) fn open_code_schema<T: Serialize>(summary: &str, known: &[T]) -> RefOr<Schema> {
    let names: Vec<String> = known
        .iter()
        .map(
            |code| match serde_json::to_value(code).expect("known codes serialize") {
                serde_json::Value::String(name) => format!("`{name}`"),
                other => panic!("codes serialize as strings, got {other}"),
            },
        )
        .collect();
    ObjectBuilder::new()
        .schema_type(Type::String)
        .description(Some(format!(
            "{summary}\n\nKnown values: {}. The set is open: clients must accept \
             any other string (spec section 4).",
            names.join(", ")
        )))
        .into()
}
