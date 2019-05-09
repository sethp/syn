use crate::operand::{Borrowed, Operand, Owned};
use crate::{file, full, gen};
use proc_macro2::{Ident, Span, TokenStream};
use quote::{quote, TokenStreamExt};
use syn::Index;
use syn_codegen::{Data, Definitions, Features, Node, Type};

const VISIT_MUT_SRC: &str = "../src/gen/visit_mut.rs";

#[derive(Default)]
struct State {
    visit_mut_trait: TokenStream,
    visit_mut_impl: TokenStream,
}

fn simple_visit(item: &str, name: &Operand) -> TokenStream {
    let ident = gen::under_name(item);
    let method = Ident::new(&format!("visit_{}_mut", ident), Span::call_site());
    let name = name.ref_mut_tokens();
    quote! {
        _visitor.#method(#name)
    }
}

fn noop_visit(name: &Operand) -> TokenStream {
    let name = name.tokens();
    quote! {
        skip!(#name)
    }
}

fn visit(
    ty: &Type,
    features: &Features,
    defs: &Definitions,
    name: &Operand,
) -> Option<TokenStream> {
    match ty {
        Type::Box(t) => {
            let name = name.owned_tokens();
            visit(t, features, defs, &Owned(quote!(*#name)))
        }
        Type::Vec(t) => {
            let operand = Borrowed(quote!(it));
            let val = visit(t, features, defs, &operand)?;
            let name = name.ref_mut_tokens();
            Some(quote! {
                for it in #name {
                    #val
                }
            })
        }
        Type::Punctuated(p) => {
            let operand = Borrowed(quote!(it));
            let val = visit(&p.element, features, defs, &operand)?;
            let name = name.ref_mut_tokens();
            Some(quote! {
                for mut el in Punctuated::pairs_mut(#name) {
                    let it = el.value_mut();
                    #val
                }
            })
        }
        Type::Option(t) => {
            let it = Borrowed(quote!(it));
            let val = visit(t, features, defs, &it)?;
            let name = name.owned_tokens();
            Some(quote! {
                if let Some(ref mut it) = #name {
                    #val
                }
            })
        }
        Type::Tuple(t) => {
            let mut code = TokenStream::new();
            for (i, elem) in t.iter().enumerate() {
                let name = name.tokens();
                let i = Index::from(i);
                let it = Owned(quote!((#name).#i));
                let val = visit(elem, features, defs, &it).unwrap_or_else(|| noop_visit(&it));
                code.append_all(val);
                code.append_all(quote!(;));
            }
            Some(code)
        }
        Type::Token(t) => {
            let name = name.tokens();
            let repr = &defs.tokens[t];
            let is_keyword = repr.chars().next().unwrap().is_alphabetic();
            let spans = if is_keyword {
                quote!(span)
            } else {
                quote!(spans)
            };
            Some(quote! {
                tokens_helper(_visitor, &mut #name.#spans)
            })
        }
        Type::Group(_) => {
            let name = name.tokens();
            Some(quote! {
                tokens_helper(_visitor, &mut #name.span)
            })
        }
        Type::Syn(t) => {
            fn requires_full(features: &Features) -> bool {
                features.any.contains("full") && features.any.len() == 1
            }
            let mut res = simple_visit(t, name);
            let target = defs.types.iter().find(|ty| ty.ident == *t).unwrap();
            if requires_full(&target.features) && !requires_full(features) {
                res = quote!(full!(#res));
            }
            Some(res)
        }
        Type::Ext(t) if gen::TERMINAL_TYPES.contains(&&t[..]) => Some(simple_visit(t, name)),
        Type::Ext(_) | Type::Std(_) => None,
    }
}

fn visit_features(features: &Features) -> TokenStream {
    let features = &features.any;
    match features.len() {
        0 => quote!(),
        1 => quote!(#[cfg(feature = #(#features)*)]),
        _ => quote!(#[cfg(any(#(feature = #features),*))]),
    }
}

fn node(state: &mut State, s: &Node, defs: &Definitions) {
    let features = visit_features(&s.features);
    let under_name = gen::under_name(&s.ident);
    let ty = Ident::new(&s.ident, Span::call_site());
    let visit_mut_fn = Ident::new(&format!("visit_{}_mut", under_name), Span::call_site());

    let mut visit_mut_impl = TokenStream::new();

    match &s.data {
        Data::Enum(variants) => {
            let mut visit_mut_variants = TokenStream::new();

            for (variant, fields) in variants {
                let variant_ident = Ident::new(variant, Span::call_site());

                if fields.is_empty() {
                    visit_mut_variants.append_all(quote! {
                        #ty::#variant_ident => {}
                    });
                } else {
                    let mut bind_visit_mut_fields = TokenStream::new();
                    let mut visit_mut_fields = TokenStream::new();

                    for (idx, ty) in fields.iter().enumerate() {
                        let name = format!("_binding_{}", idx);
                        let binding = Ident::new(&name, Span::call_site());

                        bind_visit_mut_fields.append_all(quote! {
                            ref mut #binding,
                        });

                        let borrowed_binding = Borrowed(quote!(#binding));

                        visit_mut_fields.append_all(
                            visit(ty, &s.features, defs, &borrowed_binding)
                                .unwrap_or_else(|| noop_visit(&borrowed_binding)),
                        );

                        visit_mut_fields.append_all(quote!(;));
                    }

                    visit_mut_variants.append_all(quote! {
                        #ty::#variant_ident(#bind_visit_mut_fields) => {
                            #visit_mut_fields
                        }
                    });
                }
            }

            visit_mut_impl.append_all(quote! {
                match *_i {
                    #visit_mut_variants
                }
            });
        }
        Data::Struct(fields) => {
            for (field, ty) in fields {
                let id = Ident::new(&field, Span::call_site());
                let ref_toks = Owned(quote!(_i.#id));
                let visit_mut_field = visit(&ty, &s.features, defs, &ref_toks)
                    .unwrap_or_else(|| noop_visit(&ref_toks));
                visit_mut_impl.append_all(quote! {
                    #visit_mut_field;
                });
            }
        }
        Data::Private => {}
    }

    state.visit_mut_trait.append_all(quote! {
        #features
        fn #visit_mut_fn(&mut self, i: &mut #ty) {
            #visit_mut_fn(self, i)
        }
    });

    state.visit_mut_impl.append_all(quote! {
        #features
        pub fn #visit_mut_fn<V: VisitMut + ?Sized>(
            _visitor: &mut V, _i: &mut #ty
        ) {
            #visit_mut_impl
        }
    });
}

pub fn generate(defs: &Definitions) {
    let state = gen::traverse(defs, node);
    let full_macro = full::get_macro();
    let visit_mut_trait = state.visit_mut_trait;
    let visit_mut_impl = state.visit_mut_impl;
    file::write(
        VISIT_MUT_SRC,
        quote! {
            use *;
            #[cfg(any(feature = "full", feature = "derive"))]
            use punctuated::Punctuated;
            use proc_macro2::Span;
            #[cfg(any(feature = "full", feature = "derive"))]
            use gen::helper::visit_mut::*;

            #full_macro

            #[cfg(any(feature = "full", feature = "derive"))]
            macro_rules! skip {
                ($($tt:tt)*) => {};
            }

            /// Syntax tree traversal to mutate an exclusive borrow of a syntax tree in
            /// place.
            ///
            /// See the [module documentation] for details.
            ///
            /// [module documentation]: index.html
            ///
            /// *This trait is available if Syn is built with the `"visit-mut"` feature.*
            pub trait VisitMut {
                #visit_mut_trait
            }

            #visit_mut_impl
        },
    );
}
