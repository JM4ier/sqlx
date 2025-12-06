use std::fs;

use proc_macro2::{Ident, Span};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, LitBool, LitStr, Token};
use syn::{ExprArray, Type};

/// Macro input shared by `query!()` and `query_file!()`
pub struct QueryMacroInput {
    pub(super) sql: String,

    pub(super) src_span: Span,

    pub(super) record_type: RecordType,

    pub(super) arg_exprs: Vec<Expr>,

    pub(super) checked: bool,

    pub(super) file_path: Option<String>,
}

enum QuerySrc {
    String(String),
    File(String),
}

pub enum RecordType {
    Given(Type),
    Scalar,
    Generated,
}

impl Parse for QueryMacroInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut query_src: Option<(QuerySrc, Span)> = None;
        let mut args: Option<Vec<Expr>> = None;
        let mut record_type = RecordType::Generated;
        let mut checked = true;

        let mut expect_comma = false;

        while !input.is_empty() {
            if expect_comma {
                let _ = input.parse::<syn::token::Comma>()?;
            }

            let key: Ident = input.parse()?;

            let _ = input.parse::<syn::token::Eq>()?;

            if key == "source" {
                let span = input.span();
                let query_str = Punctuated::<LitStr, Token![+]>::parse_separated_nonempty(input)?
                    .iter()
                    .map(LitStr::value)
                    .collect();
                query_src = Some((QuerySrc::String(query_str), span));
            } else if key == "source_file" {
                let lit_str = input.parse::<LitStr>()?;
                query_src = Some((QuerySrc::File(lit_str.value()), lit_str.span()));
            } else if key == "args" {
                let exprs = input.parse::<ExprArray>()?;
                args = Some(exprs.elems.into_iter().collect())
            } else if key == "record" {
                if !matches!(record_type, RecordType::Generated) {
                    return Err(input.error("colliding `scalar` or `record` key"));
                }

                record_type = RecordType::Given(input.parse()?);
            } else if key == "scalar" {
                if !matches!(record_type, RecordType::Generated) {
                    return Err(input.error("colliding `scalar` or `record` key"));
                }

                // we currently expect only `scalar = _`
                // a `query_as_scalar!()` variant seems less useful than just overriding the type
                // of the column in SQL
                input.parse::<syn::Token![_]>()?;
                record_type = RecordType::Scalar;
            } else if key == "checked" {
                let lit_bool = input.parse::<LitBool>()?;
                checked = lit_bool.value;
            } else {
                let message = format!("unexpected input key: {key}");
                return Err(syn::Error::new_spanned(key, message));
            }

            expect_comma = true;
        }

        let (src, src_span) =
            query_src.ok_or_else(|| input.error("expected `source` or `source_file` key"))?;

        let arg_exprs = args.unwrap_or_default();

        let file_path = src.file_path(src_span)?;

        QueryMacroInput {
            sql: src.resolve(src_span)?,
            src_span,
            record_type,
            arg_exprs,
            checked,
            file_path,
        }.resolve_inline_parameters()
    }
}

/// very simplistic way to allow inline arguments in the query macro, like
/// let account_id = 5;
/// sqlx::query!("select * from accounts where id = {{account_id}}")
impl QueryMacroInput {
    pub fn resolve_inline_parameters(mut self) -> syn::Result<Self> {
        #[derive(Copy, Clone)]
        enum State {
            Sql,
            OpenBrace1,
            Interpolation,
            CloseBrace1,
        }

        let mut sql = String::with_capacity(self.sql.len());
        let mut interpolation = String::new();
        let mut state = State::Sql;

        for ch in self.sql.chars() {
            match (ch, state) {
                ('{', State::Sql) => state = State::OpenBrace1,
                (ch, State::Sql) => {
                    sql.push(ch);
                }
                ('{', State::OpenBrace1) => state = State::Interpolation,
                (ch, State::OpenBrace1) => {
                    sql.push('{');
                    sql.push(ch);
                    state = State::Sql;
                }
                ('}', State::Interpolation) => state = State::CloseBrace1,
                (ch, State::Interpolation) => {
                    interpolation.push(ch);
                }
                ('}', State::CloseBrace1) => {
                    sql += "$";
                    sql += &(self.arg_exprs.len() + 1).to_string();
                    let tokens: proc_macro2::TokenStream = interpolation.parse()?; // todo fix lost span
                    let expr: Expr = syn::parse(tokens.into())?;
                    self.arg_exprs.push(expr);
                    interpolation.clear();
                    state = State::Sql;
                }
                (ch, State::CloseBrace1) => {
                    interpolation.push('}');
                    interpolation.push(ch);
                    state = State::Interpolation;
                }
            }
        }

        assert!(matches!(state, State::Sql));

        self.sql = sql;
        Ok(self)
    }
}

impl QuerySrc {
    /// If the query source is a file, read it to a string. Otherwise return the query string.
    fn resolve(self, source_span: Span) -> syn::Result<String> {
        match self {
            QuerySrc::String(string) => Ok(string),
            QuerySrc::File(file) => read_file_src(&file, source_span),
        }
    }

    fn file_path(&self, source_span: Span) -> syn::Result<Option<String>> {
        if let QuerySrc::File(ref file) = *self {
            let path = crate::common::resolve_path(file, source_span)?
                .canonicalize()
                .map_err(|e| syn::Error::new(source_span, e))?;

            Ok(Some(
                path.to_str()
                    .ok_or_else(|| {
                        syn::Error::new(
                            source_span,
                            "query file path cannot be represented as a string",
                        )
                    })?
                    .to_string(),
            ))
        } else {
            Ok(None)
        }
    }
}

fn read_file_src(source: &str, source_span: Span) -> syn::Result<String> {
    let file_path = crate::common::resolve_path(source, source_span)?;

    fs::read_to_string(&file_path).map_err(|e| {
        syn::Error::new(
            source_span,
            format!(
                "failed to read query file at {}: {}",
                file_path.display(),
                e
            ),
        )
    })
}
