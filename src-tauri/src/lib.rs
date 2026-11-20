pub mod commands;
pub mod core;

use std::sync::Mutex;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Artwork and transcoded previews are throwaway files. Clearing
            // them at startup keeps them from accumulating across sessions —
            // they are all regenerated on demand.
            commands::clear_temp_cache();
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db_path = data_dir.join("spindex.db");
            let conn = core::db::open(&db_path)
                .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
            app.manage(commands::AppState {
                db_path: db_path.clone(),
                conn: Mutex::new(conn),
                last_play: Mutex::new((0, None)),
                play_seq: std::sync::atomic::AtomicU64::new(0),
                grid_pcm: Mutex::new(None),
            });
            app.manage(commands::AnalysisState::default());
            app.manage(commands::HashState::default());
            app.manage(commands::CopyState::default());
            app.manage(commands::BatchState::default());
            app.manage(commands::SyncState::default());
            app.manage(commands::EnhanceState::default());
            app.manage(commands::MlRestoreState::default());
            app.manage(commands::StabilizeState::default());
            let devices_state = commands::DevicesState::default();
            commands::spawn_device_watcher(
                app.handle().clone(),
                db_path,
                devices_state.drives.clone(),
            );
            app.manage(devices_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_libraries,
            commands::add_library,
            commands::scan_library,
            commands::list_tracks,
            commands::list_crates,
            commands::eject_device,
            commands::forget_device,
            commands::remove_library,
            commands::import_file,
            commands::create_crate,
            commands::rename_crate,
            commands::delete_crate,
            commands::move_tracks_to_folder,
            commands::move_crate_tracks,
            commands::reorder_crate_tracks,
            commands::sort_crate_tracks,
            commands::add_tracks_to_crate,
            commands::list_serato_crates,
            commands::list_rekordbox_crates,
            commands::serato_add_tracks,
            commands::serato_remove_tracks,
            commands::serato_reorder_tracks,
            commands::serato_sort_tracks,
            commands::serato_move_tracks,
            commands::serato_create_crate,
            commands::serato_rename_crate,
            commands::serato_delete_crate,
            commands::serato_sync_preview,
            commands::serato_sync_apply,
            commands::analyzer_available,
            commands::start_analysis,
            commands::cancel_analysis,
            commands::update_track_metadata,
            commands::fetch_online_metadata,
            commands::artwork_path,
            commands::set_artwork,
            commands::remove_artwork,
            commands::prepare_playback,
            commands::get_waveform,
            commands::waveform_bands,
            commands::beat_grid,
            commands::downbeat_grid,
            commands::quality_detail,
            commands::mark_missing,
            commands::start_hashing,
            commands::cancel_hashing,
            commands::compare_library,
            commands::compare_details,
            commands::list_track_sets,
            commands::compare_set,
            commands::compare_set_details,
            commands::compare_set_unmatched,
            commands::find_duplicates_in_set,
            commands::list_devices,
            commands::add_device_libraries,
            commands::find_duplicates,
            commands::library_health,
            commands::list_genres,
            commands::create_library,
            commands::copy_tracks,
            commands::cancel_copy,
            commands::copy_crate,
            commands::delete_tracks,
            commands::show_in_explorer,
            commands::reveal_path,
            commands::update_tracks_metadata,
            commands::repair_folder_crate_tags,
            commands::repair_serato_crate_tags,
            commands::sync_preview,
            commands::list_prepared_rekordbox,
            commands::sync_to_rekordbox,
            commands::cancel_sync,
            commands::rekordbox_import_preview,
            commands::rekordbox_import,
            commands::cancel_rekordbox_import,
            commands::regenerate_serato_grid,
            commands::track_grid,
            commands::grid_waveform,
            commands::grid_colours,
            commands::save_track_grid,
            commands::regenerate_folder_crate_grids,
            commands::regenerate_serato_crate_grids,
            commands::cancel_batch,
            commands::enhance_tracks,
            commands::cancel_enhance,
            commands::restore_tracks,
            commands::apollo_ready,
            commands::ml_restore_tracks,
            commands::cancel_ml_restore,
            commands::stabilize_tracks,
            commands::cancel_stabilize
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // An Apollo inference runs ~30 minutes per track. Without this,
            // quitting SpinDex mid-restore detaches the Python process and it
            // keeps pegging a core with no window left to cancel it.
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(state) = app.try_state::<commands::MlRestoreState>() {
                    state.kill_child();
                }
            }
        });
}
