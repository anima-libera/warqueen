use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Implements the `NetSend` trait.
///
/// # Examples
///
/// ```rust
/// use serde::Serialize;
/// use warqueen::NetSend;
/// # use warqueen_derive::NetSend;
///
/// #[derive(Serialize, NetSend)]
/// enum MessageClientToServer {
///     HelloFromClient,
///     String(String),
/// }
/// ```
#[proc_macro_derive(NetSend)]
pub fn derive_net_send(input: TokenStream) -> TokenStream {
	let input = parse_macro_input!(input as DeriveInput);
	let ident = input.ident;
	// TODO: Mark the impl as `#[automatically_derived]`?
	quote! {
		impl warqueen::NetSend for #ident {}
	}
	.into()
}

/// Implements the `NetReceive` trait.
///
/// # Examples
///
/// ```rust
/// use serde::Deserialize;
/// use warqueen::NetReceive;
/// # use warqueen_derive::NetReceive;
///
/// #[derive(Deserialize, NetReceive)]
/// enum MessageServerToClient {
///     HelloFromServer,
///     String(String),
/// }
/// ```
#[proc_macro_derive(NetReceive)]
pub fn derive_net_receive(input: TokenStream) -> TokenStream {
	let input = parse_macro_input!(input as DeriveInput);
	let ident = input.ident;
	// TODO: Mark the impl as `#[automatically_derived]`?
	quote! {
		impl warqueen::NetReceive for #ident {}
	}
	.into()
}
