//! Opens a file kept on the phone (one of the family's Records) in the
//! phone's own apps: on Android, "Open with" lists the apps for that kind of
//! file; on an iPhone, Quick Look shows it, with Share to send it to another
//! app. Nothing on a computer: there the home computer opens its own files.
use std::path::Path;

#[cfg(mobile)]
use tauri::Manager as _;
use tauri::{Runtime, plugin::TauriPlugin};

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_open_record);

/// The plugin's handle to its native part, kept as app state.
#[cfg(mobile)]
struct Native<R: Runtime>(tauri::plugin::PluginHandle<R>);

/// What the native part is given.
#[cfg(mobile)]
#[derive(serde::Serialize)]
struct OpenArgs<'a> {
    /// The file.
    path: &'a str,
}

/// Opens `path` in the phone's own app for its kind of file.
///
/// # Errors
/// Not on a phone, no app opens that kind of file, or the file can't be read.
pub fn open<R: Runtime>(app: &tauri::AppHandle<R>, path: &Path) -> Result<(), String> {
    #[cfg(mobile)]
    {
        let native = app
            .try_state::<Native<R>>()
            .ok_or("The phone's file opener isn't ready")?;
        let path = path.to_str().ok_or("That file's name can't be read")?;
        native
            .0
            .run_mobile_plugin::<()>("open", OpenArgs { path })
            .map_err(|e| e.to_string())
    }
    #[cfg(not(mobile))]
    {
        let _ = (app, path);
        Err("Opening a kept file is for the phone app".to_owned())
    }
}

/// The plugin, to add to the app's builder.
#[must_use]
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    tauri::plugin::Builder::new("open-record")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            let handle = api.register_android_plugin("dev.oxbot.openrecord", "OpenRecordPlugin")?;
            #[cfg(target_os = "ios")]
            let handle = api.register_ios_plugin(init_plugin_open_record)?;
            #[cfg(mobile)]
            app.manage(Native(handle));
            #[cfg(not(mobile))]
            let _ = (app, api);
            Ok(())
        })
        .build()
}
