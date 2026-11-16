// SPDX-License-Identifier: GNU GENERAL PUBLIC LICENSE Version 3
//
// Copyright (c) 2027 James Wong. This file is part of James Wong.
// is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// James Wong is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with James Wong.  If not, see <https://www.gnu.org/licenses/>.
//
// IMPORTANT: Any software that fully or partially contains or uses materials
// covered by this license must also be released under the GNU GPL license.
// This includes modifications and derived works.

use std::collections::HashMap;

use proc_macro2::Span;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::token::Comma;
use syn::FnArg;
use syn::Ident;
use syn::Meta;
use syn::MetaNameValue;
use syn::NestedMeta;
use syn::Type;

/// Extract a String <-> Ident map from the attribute args.
pub(crate) fn extract_arg_map(args: Vec<NestedMeta>) -> Result<HashMap<String, Ident>, syn::Error> {
    args.into_iter()
        .map(|meta| {
            if let NestedMeta::Meta(Meta::NameValue(MetaNameValue { path, lit, .. })) = meta {
                let name = path.get_ident().unwrap().to_string();
                let ident = (match lit {
                    syn::Lit::Str(lit_str) => lit_str.parse::<Ident>(),
                    _ => Err(syn::Error::new(
                        lit.span(),
                        "Unexpected attribute format. Expected `name = \"value\"`",
                    )),
                })?;
                Ok((name, ident))
            } else {
                Err(syn::Error::new(
                    meta.span(),
                    "Unexpected attribute format. Expected `name = \"value\"`",
                ))
            }
        })
        .collect::<Result<HashMap<String, Ident>, syn::Error>>()
}

/// Helper function to get an Ident from the previous arg map.
pub(crate) fn get_ident(map: &HashMap<String, Ident>, key: &str, span: Span) -> Result<Ident, syn::Error> {
    map.get(key)
        .cloned()
        .ok_or_else(|| syn::Error::new(span, format!("Expect attribute {key} but not found")))
}

/// Extract the argument list from the annotated function.
pub(crate) fn extract_input_types(inputs: &Punctuated<FnArg, Comma>) -> Result<Vec<Type>, syn::Error> {
    inputs
        .iter()
        .map(|arg| match arg {
            FnArg::Receiver(receiver) => Err(syn::Error::new(receiver.span(), "expected bool")),
            FnArg::Typed(pat_type) => Ok(*pat_type.ty.clone()),
        })
        .collect()
}
