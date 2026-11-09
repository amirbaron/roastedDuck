//! Formatting of plain native fields without forcing DuckDB names into UTF-8.
//! Numeric/text flags can be prepared with Rust's standard format! macro.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Ident, LitStr, Result, Token};

struct Argument {
    name: Option<Ident>,
    value: Expr,
}

struct Format {
    literal: LitStr,
    arguments: Vec<Argument>,
}

impl Parse for Format {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let literal = input.parse()?;
        let mut arguments = Vec::new();
        let mut named = false;
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let name = if input.peek(Ident) && input.peek2(Token![=]) {
                let name: Ident = input.parse()?;
                input.parse::<Token![=]>()?;
                named = true;
                Some(name)
            } else {
                if named {
                    return Err(input.error("positional argument follows named argument"));
                }
                None
            };
            arguments.push(Argument {
                name,
                value: input.parse()?,
            });
        }
        Ok(Self { literal, arguments })
    }
}

fn expand(format: &Format) -> Result<Tokens> {
    let mut characters = format
        .literal
        .value()
        .chars()
        .collect::<Vec<_>>()
        .into_iter()
        .peekable();
    let output = format_ident!("__roastedduck_byte_output", span = Span::mixed_site());
    let mut literal = String::new();
    let mut body = Tokens::new();
    let mut implicit = 0usize;
    let mut used = vec![false; format.arguments.len()];
    let bindings: Vec<_> = (0..format.arguments.len())
        .map(|i| format_ident!("__roastedduck_byte_argument_{i}", span = Span::mixed_site()))
        .collect();
    let flush = |literal: &mut String, body: &mut Tokens| {
        if literal.is_empty() {
            return;
        }
        let text = LitStr::new(literal, format.literal.span());
        body.extend(quote! { #output.push_bytes(#text); });
        literal.clear();
    };
    while let Some(character) = characters.next() {
        match character {
            '{' if characters.peek() == Some(&'{') => {
                characters.next();
                literal.push('{');
            }
            '}' if characters.peek() == Some(&'}') => {
                characters.next();
                literal.push('}');
            }
            '}' => return Err(syn::Error::new(format.literal.span(), "unmatched closing brace")),
            '{' => {
                flush(&mut literal, &mut body);
                let mut selector = String::new();
                let mut closed = false;
                for next in characters.by_ref() {
                    if next == '}' {
                        closed = true;
                        break;
                    }
                    if matches!(next, ':' | '{') {
                        return Err(syn::Error::new(
                            format.literal.span(),
                            "native byte fields use plain {} or {name}; prepare text/numeric flags with format!",
                        ));
                    }
                    selector.push(next);
                }
                if !closed {
                    return Err(syn::Error::new(format.literal.span(), "unclosed field"));
                }
                let positional = selector.is_empty() || selector.parse::<usize>().is_ok();
                let index = if selector.is_empty() {
                    let index = implicit;
                    implicit += 1;
                    Some(index)
                } else if let Ok(index) = selector.parse::<usize>() {
                    Some(index)
                } else {
                    format
                        .arguments
                        .iter()
                        .position(|argument| argument.name.as_ref().is_some_and(|name| name == &selector))
                };
                let argument = if let Some(index) = index {
                    if index >= bindings.len() || (positional && format.arguments[index].name.is_some()) {
                        return Err(syn::Error::new(
                            format.literal.span(),
                            "missing positional argument",
                        ));
                    }
                    used[index] = true;
                    let binding = &bindings[index];
                    quote! { #binding }
                } else {
                    let ident: Ident = syn::parse_str(&selector)
                        .map_err(|_| syn::Error::new(format.literal.span(), "invalid captured field name"))?;
                    quote! { &#ident }
                };
                body.extend(quote! {
                    (&::roastedduck_common::byte_format::FormatArgument(#argument)).append_bytes(&mut #output);
                });
            }
            other => literal.push(other),
        }
    }
    flush(&mut literal, &mut body);
    if let Some(index) = used.iter().position(|used| !used) {
        return Err(syn::Error::new_spanned(
            &format.arguments[index].value,
            "unused byte-format argument",
        ));
    }
    let arguments = format.arguments.iter().map(|argument| &argument.value);
    Ok(quote! {
        match (#(&(#arguments),)*) {
            (#(#bindings,)*) => {
                use ::roastedduck_common::byte_format::AppendBytes as _;
                let mut #output = ::roastedduck_common::ByteString::new();
                #body
                #output
            }
        }
    })
}

/// Plain positional, indexed and captured/named fields with native byte dispatch.
#[proc_macro]
pub fn byte_format(input: TokenStream) -> TokenStream {
    let format = syn::parse_macro_input!(input as Format);
    expand(&format)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

struct WriteFormat {
    writer: Expr,
    format: Format,
}
impl Parse for WriteFormat {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let writer = input.parse()?;
        input.parse::<Token![,]>()?;
        Ok(Self {
            writer,
            format: input.parse()?,
        })
    }
}

/// Appends native byte fields to a `ByteString`, with the result shape of `write!`.
#[proc_macro]
pub fn byte_write(input: TokenStream) -> TokenStream {
    let WriteFormat { writer, format } = syn::parse_macro_input!(input as WriteFormat);
    match expand(&format) {
        Ok(output) => quote! {{
            (#writer).push_bytes(#output);
            ::std::fmt::Result::Ok(())
        }}
        .into(),
        Err(error) => error.into_compile_error().into(),
    }
}
