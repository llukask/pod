// ==============================================================================
// Auth screen (sign-in / register)
// ==============================================================================
//
// Standalone screen rendered when there is no token in `AppState`. Mirrors
// the JS frontend's two-tab box.

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

use crate::api;
use crate::state::use_app_state;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    SignIn,
    Register,
}

#[component]
pub fn AuthScreen() -> impl IntoView {
    let mode = RwSignal::new(Mode::SignIn);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    let username_ref: NodeRef<leptos::html::Input> = NodeRef::new();
    let password_ref: NodeRef<leptos::html::Input> = NodeRef::new();

    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let username = username_ref
            .get()
            .map(|el| {
                let el: HtmlInputElement = el.unchecked_into();
                el.value().trim().to_string()
            })
            .unwrap_or_default();
        let password = password_ref
            .get()
            .map(|el| {
                let el: HtmlInputElement = el.unchecked_into();
                el.value()
            })
            .unwrap_or_default();
        if username.is_empty() || password.is_empty() {
            return;
        }

        error.set(String::new());
        busy.set(true);
        let m = mode.get_untracked();
        spawn_local(async move {
            let res = match m {
                Mode::SignIn => api::login(&username, &password).await,
                Mode::Register => api::register(&username, &password).await,
            };
            match res {
                Ok(auth) => {
                    use_app_state().set_session(auth.token, username);
                }
                Err(e) => {
                    error.set(e.0);
                    busy.set(false);
                }
            }
        });
    };

    view! {
        <div class="auth-container">
            <div class="auth-box">
                <div class="auth-header">"POD"</div>
                <div class="auth-tabs">
                    <div
                        class:auth-tab=true
                        class:active=move || mode.get() == Mode::SignIn
                        on:click=move |_| mode.set(Mode::SignIn)>
                        "Sign In"
                    </div>
                    <div
                        class:auth-tab=true
                        class:active=move || mode.get() == Mode::Register
                        on:click=move |_| mode.set(Mode::Register)>
                        "Register"
                    </div>
                </div>
                <form class="auth-form" on:submit=on_submit>
                    <div class="form-group">
                        <label class="form-label">"Username"</label>
                        <input
                            class="form-input"
                            type="text"
                            name="username"
                            autocomplete="username"
                            required=true
                            node_ref=username_ref />
                    </div>
                    <div class="form-group">
                        <label class="form-label">"Password"</label>
                        <input
                            class="form-input"
                            type="password"
                            name="password"
                            autocomplete="current-password"
                            required=true
                            node_ref=password_ref />
                    </div>
                    <button
                        class="btn btn-primary"
                        type="submit"
                        disabled=move || busy.get()>
                        {move || {
                            if busy.get() {
                                "...".to_string()
                            } else if mode.get() == Mode::SignIn {
                                "Sign In".to_string()
                            } else {
                                "Register".to_string()
                            }
                        }}
                    </button>
                    <div class="error-msg">{move || error.get()}</div>
                </form>
            </div>
        </div>
    }
}
