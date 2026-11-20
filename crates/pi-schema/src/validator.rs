//! `compile()` and `compile_coercing()`: compiles a JSON Schema into `Validator` or `CoercingValidator`.

use std::ops::Deref;
use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::checks::Check;
use crate::coerce::Coercer;
use crate::compiler::build_compiler;
use crate::explain::Explainer;
use crate::types::{AsSchema, CompileOptions, ParseResult, SchemaError, ValidationError};

/// Compiles `schema` into a [`Validator`].
///
/// Returns [`SchemaError`] if `schema` is malformed, contains an invalid ECMAScript regex, or contains an
/// unresolved `$ref` when [`CompileOptions::unresolved_refs`] is [`UnresolvedRefs::Throw`](crate::UnresolvedRefs::Throw).
pub fn compile<S: AsSchema>(schema: S, options: CompileOptions) -> Result<Validator, SchemaError> {
    let schema_val = Arc::new(schema.as_schema_value().into_owned());
    let (_, check) = build_compiler(&schema_val, &options)?;
    Ok(Validator {
        check,
        schema: schema_val,
        options,
        explainer: OnceLock::new(),
        coercer: OnceLock::new(),
    })
}

/// Compiles `schema` with coercion enabled (`coerce: true`), returning a [`CoercingValidator`].
pub fn compile_coercing<S: AsSchema>(schema: S, mut options: CompileOptions) -> Result<CoercingValidator, SchemaError> {
    options.coerce = true;
    let validator = compile(schema, options)?;
    Ok(CoercingValidator { validator })
}

/// A compiled JSON Schema validator.
///
/// `check` runs compiled `Send + Sync` closures with zero compilation state retained on the hot path.
/// Error explanation (`Explainer`) and coercion rules (`Coercer`) are built lazily on first failure or coercion call.
pub struct Validator {
    check: Check,
    schema: Arc<Value>,
    options: CompileOptions,
    explainer: OnceLock<Explainer>,
    coercer: OnceLock<Coercer>,
}

impl Validator {
    /// Fast boolean check: whether `value` satisfies the schema.
    pub fn check(&self, value: &Value) -> bool {
        let mut scope = Vec::new();
        (self.check)(value, &mut scope)
    }

    /// Validates `value`, returning `Ok(())` when valid or `Err(errors)` when invalid.
    pub fn check_result(&self, value: &Value) -> Result<(), Vec<ValidationError>> {
        if self.check(value) {
            Ok(())
        } else {
            Err(self.errors(value))
        }
    }

    /// Explains why `value` is invalid (`Vec::new()` when valid).
    pub fn errors(&self, value: &Value) -> Vec<ValidationError> {
        if self.check(value) {
            return Vec::new();
        }
        self.get_explainer().errors(value)
    }

    /// Checks `value` without coercing: returns `Ok(value.clone())` when valid or `Err(errors)` when invalid.
    pub fn validate(&self, value: &Value) -> ParseResult<Value> {
        if self.check(value) {
            Ok(value.clone())
        } else {
            Err(self.errors(value))
        }
    }

    /// Whether this validator was compiled with `coerce: true`.
    pub fn is_coercing(&self) -> bool {
        self.options.coerce
    }

    /// Coerces `value` if compiled with `coerce: true`, otherwise returns `None`.
    pub fn try_coerce(&self, value: &Value) -> Option<Value> {
        if !self.options.coerce {
            return None;
        }
        Some(self.get_coercer().coerce(value))
    }

    /// Coerces and validates `value` if compiled with `coerce: true`, otherwise returns `None`.
    pub fn try_parse(&self, value: &Value) -> Option<ParseResult<Value>> {
        if !self.options.coerce {
            return None;
        }
        let coerced = self.get_coercer().coerce(value);
        Some(if self.check(&coerced) {
            Ok(coerced)
        } else {
            Err(self.errors(&coerced))
        })
    }

    /// Converts this validator into a [`CoercingValidator`] when compiled with `coerce: true`.
    pub fn into_coercing(self) -> Option<CoercingValidator> {
        if self.options.coerce {
            Some(CoercingValidator { validator: self })
        } else {
            None
        }
    }

    fn get_explainer(&self) -> &Explainer {
        self.explainer.get_or_init(|| {
            // Re-compiling a previously validated schema cannot fail.
            let (compiler, _) =
                build_compiler(&self.schema, &self.options).expect("compiled schema must remain valid for explainer");
            Explainer::new(compiler)
        })
    }

    fn get_coercer(&self) -> &Coercer {
        self.coercer.get_or_init(|| {
            let (compiler, _) =
                build_compiler(&self.schema, &self.options).expect("compiled schema must remain valid for coercer");
            Coercer::new(compiler)
        })
    }
}

/// A compiled JSON Schema validator with coercion enabled (`coerce` and `parse`).
pub struct CoercingValidator {
    validator: Validator,
}

impl CoercingValidator {
    /// Coerces `value` toward the schema without modifying the input.
    pub fn coerce(&self, value: &Value) -> Value {
        self.validator.get_coercer().coerce(value)
    }

    /// Coerces `value` and validates the coerced result.
    pub fn parse(&self, value: &Value) -> ParseResult<Value> {
        let coerced = self.coerce(value);
        if self.validator.check(&coerced) {
            Ok(coerced)
        } else {
            Err(self.validator.errors(&coerced))
        }
    }

    /// Runs Pass 2 (TypeBox `Value.Convert` rules) alone on `value`.
    pub fn convert_only(&self, value: &Value) -> Value {
        self.validator.get_coercer().convert_only(value)
    }

    /// Borrows the underlying [`Validator`].
    pub fn as_validator(&self) -> &Validator {
        &self.validator
    }

    /// Unwraps the underlying [`Validator`].
    pub fn into_validator(self) -> Validator {
        self.validator
    }
}

impl Deref for CoercingValidator {
    type Target = Validator;

    fn deref(&self) -> &Self::Target {
        &self.validator
    }
}
