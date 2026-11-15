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
use syn::TypeReference;
use syn::Visibility;

use crate::utils::extract_arg_map;

use crate::utils::extract_input_types;

use crate::utils::get_ident;

macro_rules! ok {
    ($item:expr) => {
        match $item {
            Ok(item) => item,
            Err(e) => return e.into_compile_error().into(),
        }
    };
}

pub(crate) fn process_range_fn(args: TokenStream, input: TokenStream) -> TokenStream {
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

    // with format like Float64Array
    let array_types = arg_types
        .iter()
        .map(|ty| {
            if let Type::Reference(TypeReference { elem, .. }) = ty {
                elem.as_ref().clone()
            } else {
                ty.clone()
            }
        })
        .collect::<Vec<_>>();

    // build the struct and its impl block
    // only do this when `display_name` is specified
    if let Ok(display_name) = get_ident(&arg_map, "display_name", arg_span) {
        let struct_code = build_struct(
            &attrs,
            &vis,
            &ok!(get_ident(&arg_map, "name", arg_span)),
            &display_name,
            &array_types,
            &ok!(get_ident(&arg_map, "ret", arg_span)),
        );
        result.extend(struct_code);
    }

    let calc_fn_code = build_calc_fn(
        &ok!(get_ident(&arg_map, "name", arg_span)),
        &arg_types,
        fn_name,
        &ok!(get_ident(&arg_map, "ret", arg_span)),
    );
    // preserve this fn, but remove its `pub` modifier
    let input_fn_code: TokenStream = (quote! {
        #sig { #block }
    })
    .into();

    result.extend(calc_fn_code);
    result.extend(input_fn_code);
    result
}

fn build_struct(
    attrs: &[Attribute],
    vis: &Visibility,
    name: &Ident,
    display_name_ident: &Ident,
    array_types: &[Type],
    return_array_type: &Ident,
) -> TokenStream {
    let display_name = display_name_ident.to_string();
    (quote! {
        #(#attrs)*
        #[derive(Debug)]
        #vis struct #name {}

        impl #name {
            pub const fn name() -> &'static str {
                #display_name
            }

            pub fn scalar_udf() -> ScalarUDF {
                ScalarUDF {
                    name: Self::name().to_string(),
                    signature: Signature::new(
                        TypeSignature::Exact(Self::input_type()),
                        Volatility::Immutable,
                    ),
                    return_type: Arc::new(|_| Ok(Arc::new(Self::return_type()))),
                    fun: Arc::new(Self::calc),
                }
            }

            fn input_type() -> Vec<DataType> {
                vec![#( RangeArray::convert_data_type(#array_types::new_null(0).data_type().clone()), )*]
            }

            fn return_type() -> DataType {
                #return_array_type::new_null(0).data_type().clone()
            }
        }
    })
    .into()
}

fn build_calc_fn(name: &Ident, param_types: &[Type], fn_name: &Ident, ret_type: &Ident) -> TokenStream {
    let param_names = param_types
        .iter()
        .enumerate()
        .map(|(i, ty)| Ident::new(&format!("param_{i}"), ty.span()))
        .collect::<Vec<_>>();
    let unref_param_types = param_types
        .iter()
        .map(|ty| {
            if let Type::Reference(TypeReference { elem, .. }) = ty {
                elem.as_ref().clone()
            } else {
                ty.clone()
            }
        })
        .collect::<Vec<_>>();
    let num_params = param_types.len();
    let param_numbers = (0..num_params).collect::<Vec<_>>();
    let range_array_names = param_names
        .iter()
        .map(|name| Ident::new(&format!("{name}_range_array"), name.span()))
        .collect::<Vec<_>>();
    let first_range_array_name = range_array_names.first().unwrap().clone();
    let first_param_name = param_names.first().unwrap().clone();

    (
        quote! {
        impl #name {
            fn calc(input: &[ColumnarValue]) -> Result<ColumnarValue, DataFusionError> {
                assert_eq!(input.len(), #num_params);

                #( let #range_array_names = RangeArray::try_new(extract_array(&input[#param_numbers])?.to_data().into())?; )*

                // check arrays len
                {
                    let len_first = #first_range_array_name.len();
                    #(
                        if len_first != #range_array_names.len() {
                            return Err(DataFusionError::Execution(format!("RangeArray have different lengths in PromQL function {}: array1={}, array2={}", #name::name(), len_first, #range_array_names.len())));
                        }
                    )*
                }

                let mut result_array = Vec::new();
                for index in 0..#first_range_array_name.len(){
                    #( let #param_names = #range_array_names.get(index).unwrap().as_any().downcast_ref::<#unref_param_types>().unwrap().clone(); )*

                    // check element len
                    {
                        let len_first = #first_param_name.len();
                        #(
                            if len_first != #param_names.len() {
                                return Err(DataFusionError::Execution(format!("RangeArray's element {} have different lengths in PromQL function {}: array1={}, array2={}", index, #name::name(), len_first, #param_names.len())));
                            }
                        )*
                    }

                    let result = #fn_name(#( &#param_names, )*);
                    result_array.push(result);
                }

                let result = ColumnarValue::Array(Arc::new(#ret_type::from_iter(result_array)));
                Ok(result)
            }
        }
    }
    ).into()
}
