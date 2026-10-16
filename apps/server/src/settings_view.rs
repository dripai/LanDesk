//! Shared GPUI settings controls; platform hosts own operations and lifecycle.
use gpui_kit::component::{
    button::*,
    input::{Input, InputState},
    tooltip::Tooltip,
    *,
};
use gpui_kit::*;

pub fn input(value: String, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| {
        let mut state = InputState::new(window, cx);
        state.set_value(value, window, cx);
        state
    })
}
pub fn connection_row(
    port: &Entity<InputState>,
    password: &Entity<InputState>,
    disabled: bool,
    save: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    refresh: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .child(div().text_sm().child("端口"))
        .child(Input::new(port).w(px(90.)).disabled(disabled))
        .child(div().text_sm().ml_2().child("密码"))
        .child(Input::new(password).flex_1().disabled(disabled))
        .child(
            Button::new("refresh-password")
                .ghost()
                .icon(gpui_kit::assets::IconName::RefreshCw)
                .tooltip("生成并保存新密码")
                .disabled(disabled)
                .on_click(refresh),
        )
        .child(
            Button::new("save")
                .primary()
                .label("保存")
                .disabled(disabled)
                .on_click(save),
        )
}
pub fn status_bar(status: &str, cx: &App) -> impl IntoElement {
    let tooltip = status.to_owned();
    div()
        .id("status")
        .h(px(36.))
        .flex_shrink_0()
        .border_t_1()
        .border_color(cx.theme().border)
        .px_4()
        .py_2()
        .text_sm()
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .child(div().text_ellipsis().child(status.to_owned()))
}
