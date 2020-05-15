use crate::{target::Target, util};
use proc_macro2::{Span, TokenStream};
use quote::{quote, ToTokens};
use syn::{parse_quote, Attribute, Block, Ident, ItemFn, Result, Signature, Visibility};

pub(crate) fn feature_fn_name(ident: &Ident, target: Option<&Target>) -> Ident {
    if let Some(target) = target {
        if target.has_features_specified() {
            let base = format!("{}_{}", ident, target.features_string());
            return Ident::new(&format!("{}_version", base), ident.span());
        }
    }

    // If this is a default fn, it doesn't have a dedicated static dispatcher
    Ident::new(&format!("{}_default_version", ident), ident.span())
}

pub(crate) struct Specialization {
    pub target: Target,
    pub block: Block,
    pub normalize: bool,
}

impl Specialization {
    fn make_fn(
        &self,
        vis: &Visibility,
        sig: &Signature,
        attrs: &[Attribute],
        associated: bool,
    ) -> Result<Vec<ItemFn>> {
        let target_string = self.target.target_string();
        let fn_name = feature_fn_name(&sig.ident, Some(&self.target));

        let mut target_attrs = Vec::new();
        target_attrs.push(parse_quote! { #[multiversion::target(#target_string)] });
        target_attrs.push(parse_quote! { #[inline] });
        target_attrs.push(parse_quote! { #[doc(hidden)] });
        target_attrs.extend(attrs.iter().cloned());

        // If this target doesn't have any features, treat it as a default version
        if self.target.has_features_specified() {
            // create target fn
            let target_sig = Signature {
                ident: fn_name,
                ..if self.normalize {
                    crate::util::normalize_signature(sig).0
                } else {
                    sig.clone()
                }
            };
            let target_fn = ItemFn {
                attrs: target_attrs,
                vis: vis.clone(),
                sig: target_sig,
                block: Box::new(self.block.clone()),
            };
            Ok(vec![target_fn])
        } else {
            Ok(vec![ItemFn {
                attrs: target_attrs,
                vis: vis.clone(),
                sig: Signature {
                    ident: fn_name,
                    ..sig.clone()
                },
                block: Box::new(self.block.clone()),
            }])
        }
    }
}

pub(crate) struct Dispatcher {
    pub attrs: Vec<Attribute>,
    pub vis: Visibility,
    pub sig: Signature,
    pub specializations: Vec<Specialization>,
    pub default: Block,
    pub associated: bool,
}

impl Dispatcher {
    // Create an attribute that disables an expression if we're on an architecture with a
    // specialized default
    fn cfg_if_not_defaulted(&self) -> Attribute {
        let mut defaulted_arches = Vec::new();
        for arches in self
            .specializations
            .iter()
            .filter_map(|Specialization { target, .. }| {
                if !target.has_features_specified() {
                    Some(target.arches_as_str())
                } else {
                    None
                }
            })
        {
            defaulted_arches.extend(arches);
        }
        parse_quote! { #[cfg(not(any(#(target_arch = #defaulted_arches),*)))] }
    }

    // Create specialized functions for arch/feature sets
    fn feature_fns(&self) -> Result<Vec<ItemFn>> {
        let mut fns = Vec::new();
        for f in &self.specializations {
            fns.extend(f.make_fn(&self.vis, &self.sig, &self.attrs, self.associated)?);
        }

        // Create default fn
        let mut attrs = self.attrs.clone();
        attrs.insert(0, parse_quote! { #[multiversion::target] });
        attrs.push(parse_quote! { #[inline(always)] });
        attrs.push(parse_quote! { #[doc(hidden)] });
        attrs.push(self.cfg_if_not_defaulted());
        fns.push({
            ItemFn {
                attrs,
                vis: self.vis.clone(),
                sig: Signature {
                    ident: feature_fn_name(&self.sig.ident, None),
                    ..self.sig.clone()
                },
                block: Box::new(self.default.clone()),
            }
        });

        Ok(fns)
    }

    fn dispatcher_fn(&self) -> Result<ItemFn> {
        let fn_params = util::fn_params(&self.sig);
        let (normalized_signature, argument_names) = util::normalize_signature(&self.sig);
        let block: Block = if cfg!(feature = "runtime_dispatch")
            && fn_params.is_empty()
            && self.sig.asyncness.is_none()
            && !util::impl_trait_present(&self.sig)
            && !self.associated
        {
            // Dispatching from an atomic fn pointer occurs when the following is true:
            //   * runtime-dispatching is enabled
            //   * the function is not generic
            //   * the function is not async
            //   * the function does not take or return an impl trait
            //   * the function is not associated
            let mut fn_ty = util::fn_type_from_signature(&self.sig)?;
            fn_ty.unsafety = fn_ty.unsafety.or(Some(parse_quote! { unsafe }));
            let feature_detection = {
                let return_if_detected =
                    self.specializations
                        .iter()
                        .filter_map(|Specialization { target, .. }| {
                            if target.has_features_specified() {
                                let target_arch = target.target_arch();
                                let features_detected = target.features_detected();
                                let function = feature_fn_name(&self.sig.ident, Some(&target));
                                Some(quote! {
                                    #target_arch
                                    {
                                        if #features_detected {
                                            return #function
                                        }
                                    }
                                })
                            } else {
                                None
                            }
                        });
                let default_fn = feature_fn_name(&self.sig.ident, None);
                quote! {
                    fn __get_fn<#(#fn_params),*>() -> #fn_ty {
                        #(#return_if_detected)*
                        #default_fn
                    };
                }
            };
            let resolver_signature = Signature {
                ident: Ident::new("__resolver_fn", Span::call_site()),
                unsafety: normalized_signature
                    .unsafety
                    .or(Some(parse_quote! { unsafe })),
                ..normalized_signature.clone()
            };
            parse_quote! {
                {
                    use core::sync::atomic::{AtomicPtr, Ordering};
                    #[cold]
                    #resolver_signature {
                        #feature_detection
                        let __current_fn = __get_fn();
                        __DISPATCHED_FN.store(__current_fn as *mut (), Ordering::Relaxed);
                        __current_fn(#(#argument_names),*)
                    }
                    static __DISPATCHED_FN: AtomicPtr<()> = AtomicPtr::new(__resolver_fn as *mut ());
                    let __current_ptr = __DISPATCHED_FN.load(Ordering::Relaxed);
                    unsafe {
                        let __current_fn = core::mem::transmute::<*mut (), #fn_ty>(__current_ptr);
                        __current_fn(#(#argument_names),*)
                    }
                }
            }
        } else {
            // Dispatch the function via branching if runtime-dispatching is disabled, or it is
            // generic/async/impl Trait
            let maybe_await = self.sig.asyncness.map(|_| util::await_tokens());
            let maybe_self = if self.associated {
                quote! { Self:: }
            } else {
                Default::default()
            };
            let return_if_detected =
                self.specializations
                    .iter()
                    .filter_map(|Specialization { target, .. }| {
                        if target.has_features_specified() {
                            let target_arch = target.target_arch();
                            let features_detected = target.features_detected();
                            let function = feature_fn_name(&self.sig.ident, Some(&target));
                            Some(quote! {
                                #target_arch
                                {
                                    if #features_detected {
                                        return unsafe { #maybe_self#function::<#(#fn_params),*>(#(#argument_names),*)#maybe_await }
                                    }
                                }
                            })
                        } else {
                            None
                        }
                    });
            let default_fn = feature_fn_name(&self.sig.ident, None);
            parse_quote! {
                {
                    #(#return_if_detected)*
                    #maybe_self#default_fn::<#(#fn_params),*>(#(#argument_names),*)#maybe_await
                }
            }
        };
        Ok(ItemFn {
            attrs: Vec::new(),
            vis: self.vis.clone(),
            sig: normalized_signature,
            block: Box::new(block),
        })
    }
}

impl ToTokens for Dispatcher {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        tokens.extend(match self.feature_fns() {
            Ok(val) => quote! { #(#val)* },
            Err(err) => err.to_compile_error(),
        });
        tokens.extend(match self.dispatcher_fn() {
            Ok(val) => val.into_token_stream(),
            Err(err) => err.to_compile_error(),
        });
    }
}
