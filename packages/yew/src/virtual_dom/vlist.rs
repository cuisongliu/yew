//! This module contains fragments implementation.
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

use super::{Key, VNode};
use crate::html::ImplicitClone;

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FullyKeyedState {
    KnownFullyKeyed,
    KnownMissingKeys,
    Unknown,
}

/// This struct represents a fragment of the Virtual DOM tree.
#[derive(Clone, Debug)]
pub struct VList {
    /// The list of child [VNode]s
    pub(crate) children: Option<Rc<Vec<VNode>>>,

    /// All [VNode]s in the VList have keys
    fully_keyed: FullyKeyedState,

    pub key: Option<Key>,
}

impl ImplicitClone for VList {}

impl PartialEq for VList {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.children == other.children
    }
}

impl Default for VList {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for VList {
    type Target = Vec<VNode>;

    fn deref(&self) -> &Self::Target {
        match self.children {
            Some(ref m) => m,
            None => {
                // This can be replaced with `const { &Vec::new() }` in Rust 1.79.
                const EMPTY: &Vec<VNode> = &Vec::new();
                EMPTY
            }
        }
    }
}

impl DerefMut for VList {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.fully_keyed = FullyKeyedState::Unknown;
        self.children_mut()
    }
}

impl VList {
    /// Creates a new empty [VList] instance.
    pub const fn new() -> Self {
        Self {
            children: None,
            key: None,
            fully_keyed: FullyKeyedState::KnownFullyKeyed,
        }
    }

    /// Creates a new [VList] instance with children.
    pub fn with_children(children: Vec<VNode>, key: Option<Key>) -> Self {
        let mut vlist = VList {
            fully_keyed: FullyKeyedState::Unknown,
            children: Some(Rc::new(children)),
            key,
        };
        vlist.recheck_fully_keyed();
        vlist
    }

    #[doc(hidden)]
    /// Used by `html!` to avoid calling `.recheck_fully_keyed()` when possible.
    pub fn __macro_new(
        children: Vec<VNode>,
        key: Option<Key>,
        fully_keyed: FullyKeyedState,
    ) -> Self {
        VList {
            children: Some(Rc::new(children)),
            fully_keyed,
            key,
        }
    }

    // Returns a mutable reference to children, allocates the children if it hasn't been done.
    //
    // This method does not reassign key state. So it should only be used internally.
    fn children_mut(&mut self) -> &mut Vec<VNode> {
        loop {
            match self.children {
                Some(ref mut m) => return Rc::make_mut(m),
                None => {
                    self.children = Some(Rc::new(Vec::new()));
                }
            }
        }
    }

    /// Add [VNode] child.
    pub fn add_child(&mut self, child: VNode) {
        if self.fully_keyed == FullyKeyedState::KnownFullyKeyed && !child.has_key() {
            self.fully_keyed = FullyKeyedState::KnownMissingKeys;
        }
        self.children_mut().push(child);
    }

    /// Add multiple [VNode] children.
    pub fn add_children(&mut self, children: impl IntoIterator<Item = VNode>) {
        let it = children.into_iter();
        let bound = it.size_hint();
        self.children_mut().reserve(bound.1.unwrap_or(bound.0));
        for ch in it {
            self.add_child(ch);
        }
    }

    /// Recheck, if the all the children have keys.
    ///
    /// You can run this, after modifying the child list through the [DerefMut] implementation of
    /// [VList], to precompute an internally kept flag, which speeds up reconciliation later.
    pub fn recheck_fully_keyed(&mut self) {
        self.fully_keyed = if self.fully_keyed() {
            FullyKeyedState::KnownFullyKeyed
        } else {
            FullyKeyedState::KnownMissingKeys
        };
    }

    pub(crate) fn fully_keyed(&self) -> bool {
        match self.fully_keyed {
            FullyKeyedState::KnownFullyKeyed => true,
            FullyKeyedState::KnownMissingKeys => false,
            FullyKeyedState::Unknown => self.iter().all(|c| c.has_key()),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::virtual_dom::{VTag, VText};

    #[test]
    fn mutably_change_children() {
        let mut vlist = VList::new();
        assert_eq!(
            vlist.fully_keyed,
            FullyKeyedState::KnownFullyKeyed,
            "should start fully keyed"
        );
        // add a child that is keyed
        vlist.add_child(VNode::VTag({
            let mut tag = VTag::new("a");
            tag.key = Some(42u32.into());
            tag.into()
        }));
        assert_eq!(
            vlist.fully_keyed,
            FullyKeyedState::KnownFullyKeyed,
            "should still be fully keyed"
        );
        assert_eq!(vlist.len(), 1, "should contain 1 child");
        // now add a child that is not keyed
        vlist.add_child(VNode::VText(VText::new("lorem ipsum")));
        assert_eq!(
            vlist.fully_keyed,
            FullyKeyedState::KnownMissingKeys,
            "should not be fully keyed, text tags have no key"
        );
        let _: &mut [VNode] = &mut vlist; // Use deref mut
        assert_eq!(
            vlist.fully_keyed,
            FullyKeyedState::Unknown,
            "key state should be unknown, since it was potentially modified through children"
        );
    }
}

#[cfg(feature = "ssr")]
mod feat_ssr {
    use std::fmt::Write;
    use std::task::Poll;

    use futures::stream::StreamExt;
    use futures::{join, pin_mut, poll, FutureExt};

    use super::*;
    use crate::feat_ssr::VTagKind;
    use crate::html::AnyScope;
    use crate::platform::fmt::{self, BufWriter};

    impl VList {
        pub(crate) async fn render_into_stream(
            &self,
            w: &mut BufWriter,
            parent_scope: &AnyScope,
            hydratable: bool,
            parent_vtag_kind: VTagKind,
        ) {
            match &self[..] {
                [] => {}
                [child] => {
                    child
                        .render_into_stream(w, parent_scope, hydratable, parent_vtag_kind)
                        .await;
                }
                _ => {
                    async fn render_child_iter<'a, I>(
                        mut children: I,
                        w: &mut BufWriter,
                        parent_scope: &AnyScope,
                        hydratable: bool,
                        parent_vtag_kind: VTagKind,
                    ) where
                        I: Iterator<Item = &'a VNode>,
                    {
                        let mut w = w;
                        while let Some(m) = children.next() {
                            let child_fur = async move {
                                // Rust's Compiler does not release the mutable reference to
                                // BufWriter until the end of the loop, regardless of whether an
                                // await statement has dropped the child_fur.
                                //
                                // We capture and return the mutable reference to avoid this.

                                m.render_into_stream(w, parent_scope, hydratable, parent_vtag_kind)
                                    .await;
                                w
                            };
                            pin_mut!(child_fur);

                            match poll!(child_fur.as_mut()) {
                                Poll::Pending => {
                                    let (mut next_w, next_r) = fmt::buffer();
                                    // Move buf writer into an async block for it to be dropped at
                                    // the end of the future.
                                    let rest_render_fur = async move {
                                        render_child_iter(
                                            children,
                                            &mut next_w,
                                            parent_scope,
                                            hydratable,
                                            parent_vtag_kind,
                                        )
                                        .await;
                                    }
                                    // boxing to avoid recursion
                                    .boxed_local();

                                    let transfer_fur = async move {
                                        let w = child_fur.await;

                                        pin_mut!(next_r);
                                        while let Some(m) = next_r.next().await {
                                            let _ = w.write_str(m.as_str());
                                        }
                                    };

                                    join!(rest_render_fur, transfer_fur);
                                    break;
                                }
                                Poll::Ready(w_) => {
                                    w = w_;
                                }
                            }
                        }
                    }

                    let children = self.iter();
                    render_child_iter(children, w, parent_scope, hydratable, parent_vtag_kind)
                        .await;
                }
            }
        }
    }
}

#[cfg(any(not(target_arch = "wasm32"), target_os = "wasi"))]
#[cfg(feature = "ssr")]
#[cfg(test)]
mod ssr_tests {
    use tokio::test;

    use crate::prelude::*;
    use crate::LocalServerRenderer as ServerRenderer;

    #[cfg_attr(not(target_os = "wasi"), test)]
    #[cfg_attr(target_os = "wasi", test(flavor = "current_thread"))]
    async fn test_text_back_to_back() {
        #[component]
        fn Comp() -> Html {
            let s = "world";

            html! { <div>{"Hello "}{s}{"!"}</div> }
        }

        let s = ServerRenderer::<Comp>::new()
            .hydratable(false)
            .render()
            .await;

        assert_eq!(s, "<div>Hello world!</div>");
    }

    #[cfg_attr(not(target_os = "wasi"), test)]
    #[cfg_attr(target_os = "wasi", test(flavor = "current_thread"))]
    async fn test_fragment() {
        #[derive(PartialEq, Properties, Debug)]
        struct ChildProps {
            name: String,
        }

        #[component]
        fn Child(props: &ChildProps) -> Html {
            html! { <div>{"Hello, "}{&props.name}{"!"}</div> }
        }

        #[component]
        fn Comp() -> Html {
            html! {
                <>
                    <Child name="Jane" />
                    <Child name="John" />
                    <Child name="Josh" />
                </>
            }
        }

        let s = ServerRenderer::<Comp>::new()
            .hydratable(false)
            .render()
            .await;

        assert_eq!(
            s,
            "<div>Hello, Jane!</div><div>Hello, John!</div><div>Hello, Josh!</div>"
        );
    }
}
