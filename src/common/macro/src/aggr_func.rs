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

use proc_macro::TokenStream;
use quote::quote;
use quote::quote_spanned;
use syn::parse::Parser;
use syn::parse_macro_input;
use syn::spanned::Spanned;
use syn::DeriveInput;
use syn::ItemStruct;

pub(crate) fn impl_aggr_func_type_store(ast: &DeriveInput) -> TokenStream {
    let name = &ast.ident;
    let gen = quote! {
        use common_query::logical_plan::accumulator::AggrFuncTypeStore;
        use common_query::error::InvalidInputStateSnafu;
        use common_query::error::Error as QueryError;
        use datatypes::prelude::ConcreteDataType;

        impl AggrFuncTypeStore for #name {
            fn input_types(&self) -> std::result::Result<Vec<ConcreteDataType>, QueryError> {
                let input_types = self.input_types.load();
                snafu::ensure!(input_types.is_some(), InvalidInputStateSnafu);
                Ok(input_types.as_ref().unwrap().as_ref().clone())
            }

            fn set_input_types(&self, input_types: Vec<ConcreteDataType>) -> std::result::Result<(), QueryError> {
                let old = self.input_types.swap(Some(std::sync::Arc::new(input_types.clone())));
                if let Some(old) = old {
                    snafu::ensure!(old.len() == input_types.len(), InvalidInputStateSnafu);
                    for (x, y) in old.iter().zip(input_types.iter()) {
                        snafu::ensure!(x == y, InvalidInputStateSnafu);
                    }
                }
                Ok(())
            }
        }
    };
    gen.into()
}

pub(crate) fn impl_as_aggr_func_creator(_args: TokenStream, input: TokenStream) -> TokenStream {
    let mut item_struct = parse_macro_input!(input as ItemStruct);
    if let syn::Fields::Named(ref mut fields) = item_struct.fields {
        let result = syn::Field::parse_named.parse2(quote! {
            input_types: arc_swap::ArcSwapOption<Vec<ConcreteDataType>>
        });
        match result {
            Ok(field) => fields.named.push(field),
            Err(e) => {
                return e.into_compile_error().into();
            }
        }
    } else {
        return quote_spanned!(
            item_struct.fields.span() => compile_error!(
                "This attribute macro needs to add fields to the its annotated struct, \
                so the struct must have \"{}\".")
        )
        .into();
    }
    (quote! {
        #item_struct
    })
    .into()
}
