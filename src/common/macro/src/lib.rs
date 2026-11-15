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

mod admin_fn;
mod aggr_func;
mod audit_log;
mod print_caller;
mod range_fn;
mod stack_trace_debug;
mod utils;

use aggr_func::impl_aggr_func_type_store;

use aggr_func::impl_as_aggr_func_creator;
use audit_log::process_audit_log;
use print_caller::process_print_caller;
use proc_macro::TokenStream;
use range_fn::process_range_fn;
use syn::parse_macro_input;
use syn::DeriveInput;

use crate::admin_fn::process_admin_fn;

/// Make struct implemented trait [`AggrFuncTypeStore`], which is necessary when writing UDAF.
/// This derive macro is expect to be used along with attribute macro [`macro@as_aggr_func_creator`].
#[proc_macro_derive(AggrFuncTypeStore)]
pub fn aggr_func_type_store_derive(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    impl_aggr_func_type_store(&ast)
}

/// A struct can be used as a creator for aggregate function if it has been annotated with this
/// attribute first. This attribute add a necessary field which is intended to store the input
/// data's types to the struct.
/// This attribute is expected to be used along with derive macro [`AggrFuncTypeStore`].
#[proc_macro_attribute]
pub fn as_aggr_func_creator(args: TokenStream, input: TokenStream) -> TokenStream {
    impl_as_aggr_func_creator(args, input)
}

/// Attribute macro to convert an arithimetic function to a range function. The annotated function
/// should accept servaral arrays as input and return a single value as output. This procedure
/// macro can works on any number of input parameters. Return type can be either primitive type
/// or wrapped in `Option`.
///
/// # Example
/// Take `count_over_time()` in `PromQL` as an example:
/// ```rust, ignore
/// /// The count of all values in the specified interval.
/// #[range_fn(
///     name = "CountOverTime",
///     ret = "Float64Array",
///     display_name = "prom_count_over_time"
/// )]
/// pub fn count_over_time(_: &TimestampMillisecondArray, values: &Float64Array) -> f64 {
///      values.len() as f64
/// }
/// ```
///
/// # Arguments
/// - `name`: The name of the generated `ScalarUDF` struct.
/// - `ret`: The return type of the generated UDF function.
/// - `display_name`: The display name of the generated UDF function.
#[proc_macro_attribute]
pub fn range_fn(args: TokenStream, input: TokenStream) -> TokenStream {
    process_range_fn(args, input)
}

/// Attribute macro to convert a normal function to SQL administration function. The annotated function
/// should accept:
///
/// - `&ProcedureServiceHandlerRef` or `&TableMutationHandlerRef` as the first argument,
/// - `&QueryContextRef` as the second argument, and
/// - `&[ValueRef<'_>]` as the third argument which is SQL function input values in each row.
///
/// Return type must be `common_query::error::Result<Value>`.
///
/// # Example see `common/function/src/system/procedure_state.rs`.
///
/// # Arguments
///
/// - `name`: The name of the generated `Function` implementation.
/// - `ret`: The return type of the generated SQL function, it will be transformed into `ConcreteDataType::{ret}_datatype()` result.
/// - `display_name`: The display name of the generated SQL function.
/// - `sig_fn`: the function to returns `Signature` of generated `Function`.
#[proc_macro_attribute]
pub fn admin_fn(args: TokenStream, input: TokenStream) -> TokenStream {
    process_admin_fn(args, input)
}

/// Attribute macro to print the caller to the annotated function.
/// The caller is printed as its filename and the call site line number.
///
/// This macro works like this: inject the tracking codes as the first statement to the annotated
/// function body. The tracking codes use [backtrace-rs](https://crates.io/crates/backtrace) to get
/// the callers. So you must dependent on the `backtrace-rs` crate.
///
/// # Arguments
/// - `depth`: The max depth of call stack to print. Optional, defaults to 1.
///
/// # Example
/// ```rust, ignore
///
/// #[print_caller(depth = 3)]
/// fn foo() {}
/// ```
#[proc_macro_attribute]
pub fn print_caller(args: TokenStream, input: TokenStream) -> TokenStream {
    process_print_caller(args, input)
}

/// Attribute macro to derive [`std::fmt::Debug`] for the annotated `Error` type.
///
/// The generated `Debug` implementation will print the error in a stack trace style. E.g.:
/// ```plaintext
/// 0: Foo error, at src/common/catalog/src/error.rs:80:10
/// 1: Bar error, at src/common/function/src/error.rs:90:10
/// 2: Root cause, invalid table name, at src/common/catalog/src/error.rs:100:10
/// ```
///
/// Notes on using this macro:
/// - `#[snafu(display)]` must present on each enum variants,
///   and should not include `location` and `source`.
/// - Only our internal error can be named `source`.
///   All external error should be `error` with an `#[snafu(source)]` annotation.
/// - `common_error` crate must be accessible.
#[proc_macro_attribute]
pub fn stack_trace_debug(args: TokenStream, input: TokenStream) -> TokenStream {
    stack_trace_debug::stack_trace_style_impl(&args.into(), &input.into()).into()
}

/// Attribute macro for automatic audit logging of function execution.
///
/// This macro wraps the annotated function to automatically log:
/// - Function entry with arguments (at INFO level by default)
/// - Function exit/return (at INFO level)
/// - Any errors (at ERROR level, if function returns Result)
///
/// # Arguments
/// - `service`: The service name for the audit log (required)
/// - `level`: Log level for entry/exit (optional, defaults to "info")
/// - `skip_args`: Comma-separated indices of arguments to skip logging (optional)
/// - `metadata`: Additional metadata to include (optional, JSON string)
///
/// # Example
/// ```rust, ignore
/// /// Log function execution with default INFO level
/// #[audit_log(service = "order-service")]
/// async fn process_order(order_id: String, quantity: u32) -> Result<Order, Error> {
///     // Function body
/// }
///
/// /// Log with WARN level and skip sensitive arguments
/// #[audit_log(service = "wallet-service", level = "warn", skip_args = "1")]
/// async fn transfer_funds(wallet_id: i64, _secret_key: &str, amount: f64) -> Result<(), Error> {
///     // Function body
/// }
///
/// /// Log with additional metadata
/// #[audit_log(service = "strategy-service", metadata = "{\"strategy_id\": \"momentum_001\"}")]
/// fn calculate_signal(price: f64, rsi: f64) -> Signal {
///     // Function body
/// }
/// ```
#[proc_macro_attribute]
pub fn audit_log(args: TokenStream, input: TokenStream) -> TokenStream {
    process_audit_log(args, input)
}
