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
use syn::parse_macro_input;
use syn::spanned::Spanned;
use syn::Attribute;
use syn::AttributeArgs;
use syn::Ident;
use syn::ItemFn;
use syn::Signature;
use syn::Type;
use syn::TypePath;
use syn::TypeReference;
use syn::Visibility;

use crate::utils::extract_arg_map;

use crate::utils::extract_input_types;

use crate::utils::get_ident;

/// Internal util macro to early return on error.
macro_rules! ok {
    ($item:expr) => {
        match $item {
            Ok(item) => item,
            Err(e) => return e.into_compile_error().into(),
        }
    };
}

/// Internal util macro to create an error.
macro_rules! error {
    ($span:expr, $msg:expr) => {
        Err(syn::Error::new($span, $msg))
    };
}

pub(crate) fn process_admin_fn(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut result = TokenStream::new();

    // extract arg map
    let arg_pairs = parse_macro_input!(args as AttributeArgs);
    let arg_span = arg_pairs[0].span();
    let arg_map = ok!(extract_arg_map(arg_pairs));

    // decompose the fn block
    let compute_fn = parse_macro_input!(input as ItemFn);
    let ItemFn { attrs, vis, sig, block } = compute_fn;

    // extract fn arg list
    let Signature {
        inputs, ident: fn_name, ..
    } = &sig;

    let arg_types = ok!(extract_input_types(inputs));
    if arg_types.len() < 2 {
        ok!(error!(
            sig.span(),
            "Expect at least two argument for admin fn: (handler, query_ctx)"
        ));
    }
    let handler_type = ok!(extract_handler_type(&arg_types));

    // build the struct and its impl block
    // only do this when `display_name` is specified
    if let Ok(display_name) = get_ident(&arg_map, "display_name", arg_span) {
        let struct_code = build_struct(
            &attrs,
            &vis,
            fn_name,
            &ok!(get_ident(&arg_map, "name", arg_span)),
            &ok!(get_ident(&arg_map, "sig_fn", arg_span)),
            &ok!(get_ident(&arg_map, "ret", arg_span)),
            handler_type,
            &display_name,
        );
        result.extend(struct_code);
    }

    // preserve this fn
    let input_fn_code: TokenStream = (quote! {
        #sig { #block }
    })
    .into();

    result.extend(input_fn_code);
    result
}

/// Retrieve the handler type, `ProcedureServiceHandlerRef` or `TableMutationHandlerRef`.
fn extract_handler_type(arg_types: &[Type]) -> Result<&Ident, syn::Error> {
    match &arg_types[0] {
        Type::Reference(TypeReference { elem, .. }) => match &**elem {
            Type::Path(TypePath { path, .. }) => {
                Ok(&path.segments.first().expect("Expected a reference of handler").ident)
            }
            other => {
                error!(other.span(), "Expected a reference of handler")
            }
        },
        other => {
            error!(other.span(), "Expected a reference of handler")
        }
    }
}

/// Build the function struct
#[allow(clippy::too_many_arguments)]
fn build_struct(
    attrs: &[Attribute],
    vis: &Visibility,
    fn_name: &Ident,
    name: &Ident,
    sig_fn: &Ident,
    ret: &Ident,
    handler_type: &Ident,
    display_name_ident: &Ident,
) -> TokenStream {
    let display_name = display_name_ident.to_string();
    let ret = Ident::new(&format!("{ret}_datatype"), ret.span());
    let uppcase_display_name = display_name.to_uppercase();
    // Get the handler name in function state by the argument ident
    let (handler, snafu_type) = match handler_type.to_string().as_str() {
        "ProcedureServiceHandlerRef" => (
            Ident::new("procedure_service_handler", handler_type.span()),
            Ident::new("MissingProcedureServiceHandlerSnafu", handler_type.span()),
        ),

        "TableMutationHandlerRef" => (
            Ident::new("table_mutation_handler", handler_type.span()),
            Ident::new("MissingTableMutationHandlerSnafu", handler_type.span()),
        ),
        handler => ok!(error!(handler_type.span(), format!("Unknown handler type: {handler}"))),
    };

    (quote! {
        #(#attrs)*
        #[derive(Debug)]
        #vis struct #name;

        impl fmt::Display for #name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, #uppcase_display_name)
            }
        }


        impl Function for #name {
            fn name(&self) -> &'static str {
                #display_name
            }

            fn return_type(&self, _input_types: &[ConcreteDataType]) -> Result<ConcreteDataType> {
                Ok(ConcreteDataType::#ret())
            }

            fn signature(&self) -> Signature {
                #sig_fn()
            }

            fn eval(&self, func_ctx: FunctionContext, columns: &[VectorRef]) ->  Result<VectorRef> {
                // Ensure under the `sigbot` catalog for security
                ensure_sigbot!(func_ctx);

                let columns_num = columns.len();
                let rows_num = if columns.is_empty() {
                    1
                } else {
                    columns[0].len()
                };
                let columns = Vec::from(columns);

                // TODO(dennis): DataFusion doesn't support async UDF currently
                std::thread::spawn(move || {
                    let query_ctx = &func_ctx.query_ctx;
                    let handler = func_ctx
                        .state
                        .#handler
                        .as_ref()
                        .context(#snafu_type)?;

                    let mut builder = ConcreteDataType::#ret()
                        .create_mutable_vector(rows_num);

                    if columns_num == 0 {
                        let result = common_runtime::block_on_read(async move {
                            #fn_name(handler, query_ctx, &[]).await
                        })?;

                        builder.push_value_ref(result.as_value_ref());
                    } else {
                        for i in 0..rows_num {
                            let args: Vec<_> = columns.iter()
                                .map(|vector| vector.get_ref(i))
                                .collect();

                            let result = common_runtime::block_on_read(async move {
                                #fn_name(handler, query_ctx, &args).await
                            })?;

                            builder.push_value_ref(result.as_value_ref());
                        }
                    }

                    Ok(builder.to_vector())
                })
                    .join()
                    .map_err(|e| {
                        error!(e; "Join thread error");
                        ThreadJoin {
                            location: Location::default(),
                        }
                    })?

            }

        }
    })
    .into()
}
