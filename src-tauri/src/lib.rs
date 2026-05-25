pub mod commands;
pub mod history;
pub mod introspect;
pub mod parse;
pub mod pg;
pub mod query;
pub mod registry;
pub mod saved;
pub mod slots;
pub mod store;
pub mod openbao;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle();
            let pool = tauri::async_runtime::block_on(store::open(handle))?;
            app.manage(pool);
            app.manage(registry::ServerRegistry::default());
            app.manage(query::ResultRegistry::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_connections,
            commands::save_connection,
            commands::update_connection,
            commands::delete_connection,
            commands::connect_server,
            commands::disconnect_server,
            commands::run_query,
            commands::get_slot_state,
            commands::list_databases,
            commands::list_schemas,
            commands::list_relations,
            commands::list_functions,
            commands::refresh_schema_cache,
            commands::cancel_query,
            commands::get_schema_payload,
            commands::fetch_more,
            commands::close_result,
            commands::analyze_completion,
            commands::list_history,
            commands::clear_history,
            commands::list_saved,
            commands::save_query,
            commands::delete_saved,
            commands::rename_saved,
            commands::write_text_file,
            commands::login_openbao,
            commands::clear_openbao_token,
            commands::set_setting,
            commands::get_setting,
            commands::get_all_settings,
            commands::refresh_openbao_creds,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
