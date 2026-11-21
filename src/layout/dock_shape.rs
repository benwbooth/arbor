// arbor: dock tree shapes, for session restore. Included from dock.rs (needs its private items).

/// The shape of a dock tree with windows as leaves: what session restore saves and rebuilds.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DockShape<Id> {
    Split {
        /// Children stacked top to bottom (otherwise side by side).
        vertical: bool,
        fractions: Vec<f64>,
        children: Vec<DockShape<Id>>,
    },
    Leaf {
        /// Windows of the tab group, in tab order (one for a plain leaf).
        tabs: Vec<Id>,
        /// Index of the shown tab.
        active: usize,
    },
}

impl<Id: Clone + PartialEq> DockShape<Id> {
    /// Maps the ids, dropping leaves (and then splits) left without any.
    pub fn filter_map<T>(&self, f: &dyn Fn(&Id) -> Option<T>) -> Option<DockShape<T>> {
        match self {
            DockShape::Leaf { tabs, active } => {
                let shown = tabs.get(*active);
                let mut new_active = 0;
                let mut new_tabs = Vec::new();
                for id in tabs {
                    if let Some(t) = f(id) {
                        if Some(id) == shown {
                            new_active = new_tabs.len();
                        }
                        new_tabs.push(t);
                    }
                }
                (!new_tabs.is_empty()).then_some(DockShape::Leaf {
                    tabs: new_tabs,
                    active: new_active,
                })
            }
            DockShape::Split {
                vertical,
                fractions,
                children,
            } => {
                let mut new_children = Vec::new();
                let mut new_fractions = Vec::new();
                for (child, frac) in zip(children, fractions) {
                    if let Some(child) = child.filter_map(f) {
                        new_children.push(child);
                        new_fractions.push(*frac);
                    }
                }
                match new_children.len() {
                    0 => None,
                    1 => new_children.pop(),
                    n => {
                        renormalize(&mut new_fractions, n);
                        Some(DockShape::Split {
                            vertical: *vertical,
                            fractions: new_fractions,
                            children: new_children,
                        })
                    }
                }
            }
        }
    }

    pub fn ids(&self, out: &mut Vec<Id>) {
        match self {
            DockShape::Leaf { tabs, .. } => out.extend(tabs.iter().cloned()),
            DockShape::Split { children, .. } => {
                for child in children {
                    child.ids(out);
                }
            }
        }
    }
}

impl<W: LayoutElement> Node<W> {
    fn shape(&self) -> DockShape<W::Id> {
        match self {
            Node::Leaf(leaf) => DockShape::Leaf {
                tabs: leaf.tiles.iter().map(|t| t.window().id().clone()).collect(),
                active: leaf.active_tab,
            },
            Node::Split(split) => DockShape::Split {
                vertical: split.axis == Axis::Vertical,
                fractions: split.fractions.clone(),
                children: split.children.iter().map(Node::shape).collect(),
            },
        }
    }

    fn into_leaves(self, out: &mut Vec<DockLeaf<W>>) {
        match self {
            Node::Leaf(leaf) => out.push(*leaf),
            Node::Split(split) => {
                for child in split.children {
                    child.into_leaves(out);
                }
            }
        }
    }
}

impl<W: LayoutElement> DockSpace<W> {
    /// The current tree shape.
    pub fn shape(&self) -> Option<DockShape<W::Id>> {
        self.root.as_ref().map(Node::shape)
    }

    /// Rearranges the docked windows into `shape`: splits, fractions and tab groups as given.
    /// Windows in `shape` that aren't docked here are skipped; docked windows not in `shape` stay,
    /// beside the rebuilt tree.
    pub fn apply_shape(&mut self, shape: &DockShape<W::Id>) {
        self.cancel_interactive_resize();

        let present: Vec<W::Id> = self
            .leaves()
            .iter()
            .flat_map(|leaf| leaf.tiles.iter().map(|t| t.window().id().clone()))
            .collect();
        let Some(shape) = shape.filter_map(&|id| present.contains(id).then(|| id.clone())) else {
            // Nothing of the shape is docked here.
            return;
        };
        let Some(root) = self.root.take() else {
            return;
        };
        let mut leaves = Vec::new();
        root.into_leaves(&mut leaves);

        let built = self.build_from_shape(&shape, &mut leaves);
        let mut root = built;
        // Whatever is left goes beside the rebuilt tree.
        for leaf in leaves.into_iter().filter(|leaf| !leaf.tiles.is_empty()) {
            let new = Node::Leaf(Box::new(leaf));
            match &mut root {
                None => root = Some(new),
                Some(Node::Split(split)) if split.axis == Axis::Horizontal => {
                    let idx = split.children.len();
                    split.children.push(new);
                    split.fractions = fractions_after_edge_insert(&split.fractions, idx);
                }
                Some(node) => wrap_in_split(node, new, Axis::Horizontal, false),
            }
        }
        self.root = root.and_then(normalize);

        let active_ok = self.active.is_some_and(|id| self.path_of_leaf(id).is_some());
        if !active_ok {
            self.active = self.leaves().first().map(|leaf| leaf.id);
        }
        self.relayout(false, true, None);
    }

    fn build_from_shape(
        &self,
        shape: &DockShape<W::Id>,
        leaves: &mut Vec<DockLeaf<W>>,
    ) -> Option<Node<W>> {
        match shape {
            DockShape::Leaf { tabs, active } => {
                let mut tiles = Vec::new();
                let mut base: Option<DockLeaf<W>> = None;
                for id in tabs {
                    let Some(li) = leaves.iter().position(|l| l.contains(id)) else {
                        continue;
                    };
                    let ti = leaves[li].tab_of(id).unwrap();
                    let tile = leaves[li].tiles.remove(ti);
                    let leaf = &mut leaves[li];
                    leaf.active_tab = leaf.active_tab.min(leaf.tiles.len().saturating_sub(1));
                    if base.is_none() {
                        // Keep the leaf struct (id, minimized slot, indicator) of the first tab.
                        if leaves[li].tiles.is_empty() {
                            base = Some(leaves.remove(li));
                        } else {
                            base = Some(self.new_empty_leaf());
                        }
                    } else if leaves[li].tiles.is_empty() {
                        leaves.remove(li);
                    }
                    tiles.push(tile);
                }
                let mut leaf = base?;
                leaf.tiles = tiles;
                leaf.active_tab = (*active).min(leaf.tiles.len() - 1);
                Some(Node::Leaf(Box::new(leaf)))
            }
            DockShape::Split {
                vertical,
                fractions,
                children,
            } => {
                let mut nodes = Vec::new();
                let mut fracs = Vec::new();
                for (child, frac) in zip(children, fractions) {
                    if let Some(node) = self.build_from_shape(child, leaves) {
                        nodes.push(node);
                        fracs.push(*frac);
                    }
                }
                match nodes.len() {
                    0 => None,
                    1 => nodes.pop(),
                    n => {
                        renormalize(&mut fracs, n);
                        Some(Node::Split(Split {
                            axis: if *vertical {
                                Axis::Vertical
                            } else {
                                Axis::Horizontal
                            },
                            children: nodes,
                            fractions: fracs,
                            rect: Rect::default(),
                        }))
                    }
                }
            }
        }
    }
}

impl<W: LayoutElement> DockSpace<W> {
    /// A new leaf with no tiles yet (filled in by the caller).
    fn new_empty_leaf(&self) -> DockLeaf<W> {
        DockLeaf {
            id: LeafId::next(),
            tiles: Vec::new(),
            active_tab: 0,
            tab_indicator: TabIndicator::new(self.options.layout.tab_indicator),
            is_pending_fullscreen: false,
            is_pending_maximized: false,
            minimized: None,
            rect: Rect::default(),
        }
    }
}
