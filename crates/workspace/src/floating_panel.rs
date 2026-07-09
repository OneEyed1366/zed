use crate::ModalView;
use crate::WorkspaceSettings;
use crate::dock::{Panel, PanelEvent};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Pixels, Render, StyleRefinement, Styled, Subscription, Window, div,
    px,
};
use settings::{FloatingPanelSize, Settings};
use ui::prelude::*;

/// Shows a `Panel` as a centered overlay instead of docking it, closing on
/// click-away, Escape, or when the panel emits `PanelEvent::Close` (e.g. the
/// panel's own `Close` action) — the same interaction model as the file finder.
pub struct FloatingPanel<T: Panel> {
    panel: Entity<T>,
    _subscription: Subscription,
}

impl<T: Panel> FloatingPanel<T> {
    pub fn new(panel: Entity<T>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe_in(&panel, window, |_, _, event, _, cx| {
            if matches!(event, PanelEvent::Close) {
                cx.emit(DismissEvent);
            }
        });
        Self {
            panel,
            _subscription: subscription,
        }
    }
}

impl<T: Panel> EventEmitter<DismissEvent> for FloatingPanel<T> {}

impl<T: Panel> Focusable for FloatingPanel<T> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.panel.read(cx).focus_handle(cx)
    }
}

/// Width, and fixed height if any (the `Small` preset instead caps its
/// height, so it doesn't grow taller than its content needs to).
fn modal_size<T: Panel>(
    panel: &Entity<T>,
    window: &mut Window,
    cx: &mut App,
) -> (Pixels, Option<Pixels>) {
    let viewport = window.viewport_size();
    let settings = WorkspaceSettings::get_global(cx);
    match settings.floating_panel_size {
        FloatingPanelSize::Small => (panel.read(cx).default_size(window, cx), None),
        FloatingPanelSize::Medium => (viewport.width * 0.7, Some(viewport.height * 0.7)),
        FloatingPanelSize::Large => (viewport.width * 0.9, Some(viewport.height * 0.9)),
        FloatingPanelSize::Fullscreen => {
            // The default (80px) matches the `top_20()` offset the modal
            // layer already applies above this panel, so the top/left/
            // right/bottom margins all end up equal instead of the panel
            // running to the bottom edge.
            let margin = px(settings.floating_panel_padding);
            (
                viewport.width - margin * 2.,
                Some(viewport.height - margin * 2.),
            )
        }
    }
}

impl<T: Panel> Render for FloatingPanel<T> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (width, height) = modal_size(&self.panel, window, cx);
        div()
            // Close on Escape even though the wrapped panel has its own
            // `menu::Cancel` handler for panel-local state (clearing a
            // selection, discarding an edit, etc.) that would otherwise
            // consume the keystroke without dismissing the overlay. Capture
            // runs before that handler gets a chance to stop propagation.
            .capture_action({
                let panel = cx.entity().downgrade();
                move |_: &menu::Cancel, _, cx| {
                    panel.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
                }
            })
            .w(width)
            .when_some(height, |this, height| this.h(height))
            .when(height.is_none(), |this| this.max_h_128())
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .shadow_lg()
            .map(|this| {
                if height.is_some() {
                    // A fixed height was requested, so make the panel stretch
                    // to fill it instead of sizing to its own content (which
                    // would leave empty space below a short panel).
                    this.child(
                        self.panel
                            .clone()
                            .cached(StyleRefinement::default().v_flex().size_full()),
                    )
                } else {
                    this.child(self.panel.clone())
                }
            })
    }
}

impl<T: Panel> ModalView for FloatingPanel<T> {}
