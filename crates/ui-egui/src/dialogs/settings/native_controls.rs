//! Settings stays native as one interactive subtree until SDK/native traversal is supported.
//! This is presentation dispatch only: no key interception, event replay or focus substitution.
use egui::{Context, Id};

fn key(ctx: &Context) -> Id {
    Id::unique(("lightcraft-native-settings-controls", ctx.viewport_id()))
}

pub(crate) fn active(ctx: &Context) -> bool {
    let id = key(ctx);
    ctx.data(|data| data.get_temp::<bool>(id).unwrap_or(false))
}

pub(super) fn scope<R>(ctx: &Context, show: impl FnOnce() -> R) -> R {
    let id = key(ctx);
    let previous = ctx.data(|data| data.get_temp::<bool>(id));
    ctx.data_mut(|data| data.insert_temp(id, true));
    let result = show();
    ctx.data_mut(|data| {
        if let Some(previous) = previous {
            data.insert_temp(id, previous);
        } else {
            data.remove::<bool>(id);
        }
    });
    result
}

#[cfg(test)]
mod tests {
    use super::{active, scope};
    #[test]
    fn nested_native_settings_presentation_restores_the_outer_sdk_default() {
        let ctx = egui::Context::default();
        assert!(!active(&ctx));
        scope(&ctx, || {
            assert!(active(&ctx));
            scope(&ctx, || assert!(active(&ctx)));
            assert!(active(&ctx));
        });
        assert!(!active(&ctx));
    }
}

#[cfg(test)]
#[path = "mixed_native_sdk_traversal_tests.rs"]
mod mixed_native_sdk_traversal_tests;
