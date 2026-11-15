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

//! Audit Log Attribute Macro Implementation
//!
//! This module provides the implementation for the `#[audit_log]` attribute macro
//! that automatically logs function execution.

use proc_macro::TokenStream;
use quote::quote;
use syn2::parse::Parse;
use syn2::parse::ParseStream;
use syn2::parse_macro_input;
use syn2::Expr;
use syn2::FnArg;
use syn2::ItemFn;
use syn2::Lit;
use syn2::Meta;
use syn2::MetaNameValue;
use syn2::Pat;
use syn2::PatType;
use syn2::Result;
use syn2::Token;

/// Configuration for the `audit_log` attribute
struct AuditLogAttrs {
    service: Option<String>,
    level: String,
    skip_args: Vec<usize>,
    metadata: Option<String>,
}

impl Parse for AuditLogAttrs {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut service: Option<String> = None;
        let mut level = String::from("info");
        let mut skip_args = Vec::new();
        let mut metadata: Option<String> = None;

        // Check if it's a simple string literal (e.g., #[audit_log("message")])
        if input.peek(syn2::LitStr) {
            let lit: syn2::LitStr = input.parse()?;
            // Use the string literal as metadata
            metadata = Some(lit.value());
            return Ok(AuditLogAttrs {
                service,
                level,
                skip_args,
                metadata,
            });
        }

        // Otherwise parse as key=value pairs (e.g., #[audit_log(service = "xxx", level = "info")])
        let meta_list: syn2::punctuated::Punctuated<Meta, Token![,]> =
            input.parse_terminated(Meta::parse, Token![,])?;

        for meta in &meta_list {
            if let Meta::NameValue(MetaNameValue { path, value, .. }) = &meta {
                let ident = path
                    .get_ident()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_default();
                match ident.as_str() {
                    "service" => {
                        if let Expr::Lit(expr_lit) = value {
                            if let Lit::Str(lit) = &expr_lit.lit {
                                service = Some(lit.value());
                            }
                        }
                    }
                    "level" => {
                        if let Expr::Lit(expr_lit) = value {
                            if let Lit::Str(lit) = &expr_lit.lit {
                                level = lit.value();
                            }
                        }
                    }
                    "skip_args" => {
                        if let Expr::Lit(expr_lit) = value {
                            if let Lit::Str(lit) = &expr_lit.lit {
                                skip_args = lit.value().split(',').filter_map(|s| s.trim().parse().ok()).collect();
                            }
                        }
                    }
                    "metadata" => {
                        if let Expr::Lit(expr_lit) = value {
                            if let Lit::Str(lit) = &expr_lit.lit {
                                metadata = Some(lit.value());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(AuditLogAttrs {
            service,
            level,
            skip_args,
            metadata,
        })
    }
}

/// Map level string to `AuditLogLevel` path
fn level_to_path(level: &str) -> syn2::Path {
    match level.to_lowercase().as_str() {
        "debug" => syn2::parse_str("common_audit_log::AuditLogLevel::Debug").unwrap(),
        "warn" => syn2::parse_str("common_audit_log::AuditLogLevel::Warn").unwrap(),
        "error" => syn2::parse_str("common_audit_log::AuditLogLevel::Error").unwrap(),
        _ => syn2::parse_str("common_audit_log::AuditLogLevel::Info").unwrap(),
    }
}

/// Process the `audit_log` attribute macro
pub fn process_audit_log(args: TokenStream, input: TokenStream) -> TokenStream {
    let attrs = parse_macro_input!(args as AuditLogAttrs);
    let input_fn = parse_macro_input!(input as ItemFn);

    let level = &attrs.level;
    let skip_args = &attrs.skip_args;
    let metadata = attrs.metadata.as_deref().unwrap_or("");

    // Extract function signature parts
    let vis = &input_fn.vis;
    let sig = &input_fn.sig;
    let fn_name = &sig.ident;
    let inputs = &sig.inputs;
    let asyncness = &sig.asyncness;

    // Build argument names for logging - collect arg names as strings
    let mut arg_names = Vec::new();

    for (idx, input) in inputs.iter().enumerate() {
        if skip_args.contains(&idx) {
            continue;
        }

        match input {
            FnArg::Typed(PatType { pat, .. }) => {
                if let Pat::Ident(ref ident) = **pat {
                    let arg_name = &ident.ident;
                    arg_names.push(quote! { format!("{}={:?}", stringify!(#arg_name), #arg_name) });
                }
            }
            FnArg::Receiver(_) => {
                arg_names.push(quote! { "self=...".to_string() });
            }
        }
    }

    // Build the wrapped function
    let block = &input_fn.block;
    let level_path = level_to_path(level);

    // Generate service parameter - None if not specified (will use default from env/config)
    let service_param = if let Some(ref svc) = attrs.service {
        quote! { Some(#svc) }
    } else {
        quote! { None }
    };

    let wrapped_fn = if asyncness.is_some() {
        // Async function
        quote! {
            #vis #sig {
                // Log function entry using AuditGuard for RAII
                let _audit_guard = common_audit_log::AuditGuard::new(
                    #service_param,
                    #level_path,
                    format!("{}({})", stringify!(#fn_name), vec![#(#arg_names),*].join(", ")),
                    #metadata,
                );

                // Execute function body
                let result = #block;

                // Log function exit
                _audit_guard.exit();

                result
            }
        }
    } else {
        // Sync function
        quote! {
            #vis #sig {
                // Log function entry using AuditGuard for RAII
                let _audit_guard = common_audit_log::AuditGuard::new(
                    #service_param,
                    #level_path,
                    format!("{}({})", stringify!(#fn_name), vec![#(#arg_names),*].join(", ")),
                    #metadata,
                );

                // Execute function body
                let result = #block;

                // Log function exit
                _audit_guard.exit();

                result
            }
        }
    };

    wrapped_fn.into()
}
