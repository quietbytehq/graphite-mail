mod attachments;
mod drafts;
mod auth;
mod calendar;
mod contacts;
mod folders;
mod google;
mod http;
mod imap;
mod images;
mod links;
mod mail;
mod notify;
mod migrate;
mod outbox;
mod rules;
mod storage;
#[cfg(test)]
mod security_tests;
#[cfg(test)]
mod platform_tests;
#[cfg(test)]
mod personal_tests;
mod local_contacts;
mod sync;

use auth::Account;
use tauri_plugin_opener::OpenerExt;

type CmdResult<T> = Result<T, String>;

#[tauri::command]
async fn sign_in(app: tauri::AppHandle) -> CmdResult<Account> {
    auth::sign_in(|url| {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn current_account() -> Option<Account> {
    auth::current_account()
}

#[tauri::command]
fn sign_out() -> CmdResult<()> {
    for a in auth::accounts() {
        let _ = sync::forget_account_data(&a.email);
        imap::forget(&a.email);
    }
    auth::sign_out().map_err(|e| e.to_string())
}

#[tauri::command]
fn list_accounts() -> Vec<Account> {
    auth::accounts()
}

/// Removes one account: its sign-in and its downloaded mail and calendar.
#[tauri::command]
fn remove_account(email: String) -> CmdResult<()> {
    sync::forget_account_data(&email).map_err(|e| e.to_string())?;
    imap::forget(&email);
    auth::sign_out_account(Some(&email)).map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_google_account(app: tauri::AppHandle) -> CmdResult<Account> {
    google::sign_in(|url| app.opener().open_url(url, None::<&str>).map_err(|e| anyhow::anyhow!(e.to_string())))
        .await
        .map_err(|e| e.to_string())
}

/// The Contacts view: directory and personal contacts matching `query` (optionally of one account), and how each
/// Microsoft account's contacts stand (not granted yet, blocked by the organisation, …).
#[tauri::command]
fn contacts_overview(query: String, account: Option<String>) -> CmdResult<contacts::Overview> {
    contacts::overview(&query, account.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
fn contact_detail(id: String) -> CmdResult<contacts::Detail> {
    contacts::detail(&id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn refresh_contacts(account: Option<String>) -> CmdResult<Vec<contacts::AccountStatus>> {
    contacts::refresh(account.as_deref()).await.map_err(|e| e.to_string())
}

/// "Grant access": asks Microsoft for the Contacts permissions for one account (opens the browser), then syncs.
#[tauri::command]
async fn grant_contacts(app: tauri::AppHandle, email: String) -> CmdResult<Vec<contacts::AccountStatus>> {
    let acc = auth::accounts().into_iter().find(|a| a.email.eq_ignore_ascii_case(&email)).ok_or("Unknown account")?;
    if acc.provider != "microsoft" {
        return Err("Contacts are available for Microsoft 365 accounts.".into());
    }
    let open = |url: &str| app.opener().open_url(url, None::<&str>).map_err(|e| anyhow::anyhow!(e.to_string()));
    let got = auth::sign_in_scoped(open, acc.rules, true, Some(&email), acc.personal).await.map_err(|e| e.to_string())?;
    if !got.email.eq_ignore_ascii_case(&email) {
        return Err(format!("You signed in as {}, but this is for {email}. Please try again with {email}.", got.email));
    }
    contacts::refresh(Some(&email)).await.map_err(|e| e.to_string())
}

/// Forgets one account's downloaded contacts (they stay on the server) until "Refresh contacts".
#[tauri::command]
fn remove_cached_contacts(email: String) -> CmdResult<()> {
    contacts::remove_cached_for(&email).map_err(|e| e.to_string())
}

/// Details of several contacts at once (the right-click menu of a selection).
#[tauri::command]
fn contact_details(ids: Vec<String>) -> CmdResult<Vec<contacts::Detail>> {
    ids.iter().map(|id| contacts::detail(id)).collect::<anyhow::Result<_>>().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_contact_favourite(id: String, on: bool) -> CmdResult<()> {
    contacts::set_favourite(&id, on).map_err(|e| e.to_string())
}

/// Saves contacts as a vCard 3.0 file at `path` (chosen in the save dialog); returns how many.
#[tauri::command]
fn export_vcards(ids: Vec<String>, path: String) -> CmdResult<usize> {
    contacts::export_vcards(&ids, &path).map_err(|e| e.to_string())
}

/// Block sender for an address (Contacts view): a rule sending its mail to Junk.
#[tauri::command]
async fn block_address(account: String, address: String) -> CmdResult<String> {
    rules::block_address(&account, &address).await.map_err(|e| e.to_string())
}

/// "Import contacts…" (Gmail app-password accounts): a vCard or Google Contacts CSV file into the Imported group.
#[tauri::command]
fn import_contacts(account: String, path: String) -> CmdResult<local_contacts::ImportReport> {
    local_contacts::import_file(&account, &path).map_err(|e| e.to_string())
}

/// To/Cc/Bcc suggestions while writing from `account` (Microsoft 365 contacts, or the local contacts of a Gmail
/// app-password account).
#[tauri::command]
fn suggest_recipients(query: String, account: String) -> CmdResult<Vec<contacts::Suggestion>> {
    contacts::suggest(&query, &account, 8).map_err(|e| e.to_string())
}

/// Whether "Add Google account" can work in this build (false when it was built without the Google client secret).
#[tauri::command]
fn google_sign_in_available() -> bool {
    google::sign_in_available()
}

/// Gives up on a browser sign-in that is still waiting (the user cancelled, or it took too long).
#[tauri::command]
fn cancel_sign_in() {
    auth::cancel_sign_in();
}

/// Gmail with an app password (IMAP + SMTP): checks both servers accept it, then keeps it in the keyring.
/// Needs no Google sign-in client, so it works in every build.
#[tauri::command]
async fn add_imap_account(email: String, password: String, settings: Option<imap::ImapSettings>) -> CmdResult<Account> {
    imap::add_account(&email, &password, settings).await.map_err(|e| e.to_string())
}

/// The link a user can send their IT administrator to approve Graphite Mail for the organisation.
#[tauri::command]
fn admin_consent_link(email: Option<String>) -> String {
    auth::admin_consent_url(email.as_deref())
}

/// Confirms the stored session still works by silently refreshing a token.
#[tauri::command]
async fn check_session() -> CmdResult<bool> {
    Ok(auth::access_token().await.is_ok())
}

#[derive(serde::Serialize)]
struct SetupState {
    ready: bool,
    default_dir: String,
}

#[tauri::command]
fn setup_state() -> CmdResult<SetupState> {
    Ok(SetupState {
        ready: storage::open_existing().map_err(|e| e.to_string())?,
        default_dir: storage::default_data_dir(),
    })
}

#[tauri::command]
fn initialize_storage(path: String) -> CmdResult<()> {
    storage::initialize(&path).map_err(|e| e.to_string())
}

#[tauri::command]
async fn sync_now(app: tauri::AppHandle) -> CmdResult<sync::SyncReport> {
    let r = sync::sync_all().await.map_err(|e| e.to_string())?;
    notify::announce(&app);
    Ok(r)
}

#[tauri::command]
fn get_notifications() -> bool {
    notify::enabled()
}

#[tauri::command]
fn set_notifications(app: tauri::AppHandle, on: bool) -> CmdResult<()> {
    notify::set_setting("notifications", if on { "on" } else { "off" }).map_err(|e| e.to_string())?;
    if on {
        use tauri_plugin_notification::NotificationExt;
        let _ = app.notification().builder().title("Graphite Mail").body("Notifications are on. You'll see new Inbox mail here.").show();
    }
    Ok(())
}

#[tauri::command]
fn list_folders() -> CmdResult<Vec<mail::Folder>> {
    mail::folders().map_err(|e| e.to_string())
}

#[tauri::command]
fn list_messages(folder_id: String, offset: i64, limit: i64) -> CmdResult<Vec<mail::MessageRow>> {
    mail::messages(&folder_id, offset, limit).map_err(|e| e.to_string())
}

/// Whether this sender's remote images are always loaded.
#[tauri::command]
fn image_sender_allowed(address: String) -> CmdResult<bool> {
    images::allowed(&address).map_err(|e| e.to_string())
}

/// "Always load images from this sender" (allowed) or "Stop loading them".
#[tauri::command]
fn set_image_sender(address: String, allowed: bool) -> CmdResult<()> {
    images::set(&address, allowed).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_message(id: String) -> CmdResult<mail::MessageDetail> {
    // IMAP accounts download a body when it is first opened, if the background download hasn't reached it yet.
    // Offline, this gives up quickly and shows what is here; the next open tries again.
    match tokio::time::timeout(std::time::Duration::from_secs(15), imap::ensure_body(&id)).await {
        Ok(Err(e)) => eprintln!("[graphite] couldn't download the message: {e}"),
        Err(_) => eprintln!("[graphite] downloading the message took too long"),
        Ok(Ok(())) => {}
    }
    mail::message(&id).map_err(|e| e.to_string())
}

#[tauri::command]
fn search_messages(query: String) -> CmdResult<Vec<mail::MessageRow>> {
    mail::search(&query, 200).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_read(id: String, read: bool) -> CmdResult<()> {
    outbox::set_read(&id, read).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_flag(id: String, flagged: bool) -> CmdResult<()> {
    outbox::set_flag(&id, flagged).map_err(|e| e.to_string())
}

#[tauri::command]
fn move_message(id: String, folder_id: String) -> CmdResult<()> {
    outbox::move_to(&id, &folder_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_message(id: String) -> CmdResult<()> {
    outbox::delete(&id).map_err(|e| e.to_string())
}

#[tauri::command]
fn copy_message(id: String, folder_id: String) -> CmdResult<()> {
    outbox::copy_to(&id, &folder_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn mark_spam(id: String) -> CmdResult<()> {
    outbox::mark_spam(&id).map_err(|e| e.to_string())
}

/// Adds a server rule sending the message's sender to Junk / Spam; returns the blocked address.
#[tauri::command]
async fn block_sender(id: String) -> CmdResult<String> {
    rules::block_sender(&id).await.map_err(|e| e.to_string())
}

/// The colour theme: "system" (follow the desktop) or one of the named themes.
#[tauri::command]
fn get_theme() -> String {
    notify::theme()
}

#[tauri::command]
fn set_theme(theme: String) -> CmdResult<()> {
    notify::set_theme(&theme).map_err(|e| e.to_string())
}

#[tauri::command]
async fn send_mail(draft: outbox::Draft) -> CmdResult<bool> {
    outbox::send(draft).map_err(|e| e.to_string())?;
    // Try to send right away; if offline it stays queued for the next sync.
    Ok(outbox::replay().await.is_ok())
}

#[tauri::command]
fn pending_actions() -> CmdResult<i64> {
    outbox::pending_count().map_err(|e| e.to_string())
}

#[tauri::command]
fn pending_details() -> CmdResult<Vec<outbox::QueueItem>> {
    outbox::overview().map_err(|e| e.to_string())
}

/// Puts changes that were rejected back in line for another try, and tries them now.
#[tauri::command]
async fn retry_pending() -> CmdResult<()> {
    outbox::retry_failed().map_err(|e| e.to_string())?;
    outbox::replay()
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Gives up on changes the server rejected.
#[tauri::command]
fn discard_failed() -> CmdResult<()> {
    outbox::discard_failed().map_err(|e| e.to_string())
}

#[tauri::command]
fn data_folder_info() -> CmdResult<migrate::Info> {
    migrate::info().map_err(|e| e.to_string())
}

/// Moves the database and saved mail to a new folder, safely. Progress is sent as "migration-progress" events.
#[tauri::command]
async fn move_data_folder(app: tauri::AppHandle, target: String) -> CmdResult<String> {
    use tauri::Emitter;
    tauri::async_runtime::spawn_blocking(move || {
        migrate::run(&target, |p| {
            let _ = app.emit("migration-progress", p);
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_attachments(message_id: String) -> CmdResult<Vec<attachments::AttachmentRow>> {
    attachments::list(&message_id).await.map_err(|e| e.to_string())
}

/// Downloads (if needed) and opens an attachment with the system's default program.
#[tauri::command]
async fn open_attachment(app: tauri::AppHandle, id: String) -> CmdResult<()> {
    let path = attachments::prepare_open(&id).await.map_err(|e| e.to_string())?;
    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
async fn save_attachment(id: String, dest: String) -> CmdResult<String> {
    attachments::save(&id, &dest).await.map(|p| p.to_string_lossy().into_owned()).map_err(|e| e.to_string())
}

#[tauri::command]
fn save_draft(draft: drafts::DraftRow) -> CmdResult<String> {
    drafts::save(draft).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_drafts() -> CmdResult<Vec<drafts::DraftRow>> {
    drafts::list().map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_draft(id: String) -> CmdResult<()> {
    drafts::delete(&id).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_signature(email: String) -> String {
    drafts::get_signature(&email)
}

#[tauri::command]
fn set_signature(email: String, text: String) -> CmdResult<()> {
    drafts::set_signature(&email, &text).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_rules() -> CmdResult<Vec<rules::RuleView>> {
    rules::list().map_err(|e| e.to_string())
}

/// Creates a rule, or updates it when it has an id.
#[tauri::command]
async fn save_rule(rule: rules::Rule) -> CmdResult<()> {
    if rule.id.is_some() { rules::update(rule).await } else { rules::create(rule).await.map(|_| ()) }.map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_rule(id: String) -> CmdResult<()> {
    rules::delete(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_rule_enabled(id: String, on: bool) -> CmdResult<()> {
    rules::set_enabled(&id, on).await.map_err(|e| e.to_string())
}

/// Applies a rule to the mail already in a folder; returns how many messages it matched.
#[tauri::command]
async fn run_rule(id: String, folder_id: String) -> CmdResult<usize> {
    let n = rules::run_now(&id, &folder_id).map_err(|e| e.to_string())?;
    let _ = outbox::replay().await;
    Ok(n)
}

/// One-time extra permission for mail rules on one account (opens the browser).
#[tauri::command]
async fn enable_rules(app: tauri::AppHandle, email: String) -> CmdResult<Account> {
    let open = |url: &str| app.opener().open_url(url, None::<&str>).map_err(|e| anyhow::anyhow!(e.to_string()));
    let found = auth::accounts().into_iter().find(|a| a.email.eq_ignore_ascii_case(&email)).ok_or("Unknown account")?;
    if found.provider == imap::PROVIDER {
        return Ok(found); // rules of app-password accounts are kept on this computer: nothing to allow
    }
    let provider = found.provider;
    let acc = if provider == "google" { google::sign_in_with(open, true, Some(&email)).await } else { auth::sign_in_with(open, true, Some(&email)).await }.map_err(|e| e.to_string())?;
    if !acc.email.eq_ignore_ascii_case(&email) {
        return Err(format!("You signed in as {}, but this rule belongs to {email}. Please try again with {email}.", acc.email));
    }
    Ok(acc)
}

#[tauri::command]
fn list_calendars() -> CmdResult<Vec<calendar::Calendar>> {
    calendar::calendars().map_err(|e| e.to_string())
}

#[tauri::command]
fn list_events(start: i64, end: i64) -> CmdResult<Vec<calendar::Event>> {
    calendar::events(start, end).map_err(|e| e.to_string())
}

#[tauri::command]
async fn rsvp_event(id: String, response: String) -> CmdResult<()> {
    calendar::rsvp(&id, &response).map_err(|e| e.to_string())?;
    let _ = outbox::replay().await;
    Ok(())
}

#[tauri::command]
async fn create_event(event: calendar::NewEvent) -> CmdResult<String> {
    let id = calendar::create(event).map_err(|e| e.to_string())?;
    let _ = outbox::replay().await;
    Ok(id)
}

#[tauri::command]
async fn delete_event(id: String) -> CmdResult<()> {
    calendar::delete(&id).map_err(|e| e.to_string())?;
    let _ = outbox::replay().await;
    Ok(())
}

/// Opens a web link in the system browser (used by the UI, e.g. "Join meeting"). Only http/https are accepted.
#[tauri::command]
fn open_link(app: tauri::AppHandle, url: String) -> CmdResult<()> {
    let u = links::external(&url)?;
    eprintln!("[graphite] opening link: {u}");
    app.opener().open_url(u.as_str(), None::<&str>).map_err(|e| {
        eprintln!("[graphite] open_link failed: {e}");
        e.to_string()
    })
}

/// Decides every navigation request of the app window (including those from the email frame).
/// Web links open in the system browser; mailto: links start a new message; the app itself loads normally.
/// This runs inside the web engine, so it works even though scripts are switched off inside emails.
fn handle_link(app: &tauri::AppHandle, url: &tauri::Url) -> bool {
    use tauri::Emitter;
    match links::classify(url, cfg!(dev)) {
        links::Action::Allow => true,
        links::Action::OpenExternal(target) => {
            let app = app.clone();
            // Off the UI thread, so a slow browser start can never freeze the window.
            std::thread::spawn(move || {
                eprintln!("[graphite] opening link: {target}");
                if let Err(e) = app.opener().open_url(&target, None::<&str>) {
                    eprintln!("[graphite] could not open link: {e}");
                    let _ = app.emit("link-failed", target);
                }
            });
            false
        }
        links::Action::Compose(address) => {
            let _ = app.emit("mailto", address);
            false
        }
        links::Action::Deny => {
            eprintln!("[graphite] blocked a link: {}", url.scheme());
            false
        }
    }
}

fn allow_navigation(app: &tauri::AppHandle, url: &tauri::Url) -> bool {
    handle_link(app, url)
}

/// A link in an email asks for a new window: never open one, hand the address to the same checks instead.
fn new_window_request(app: &tauri::AppHandle, url: tauri::Url) -> tauri::webview::NewWindowResponse<tauri::Wry> {
    let _ = handle_link(app, &url);
    tauri::webview::NewWindowResponse::Deny
}

/// The UI has drawn its first screen; the splash may hand over to the main window.
static UI_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// How long the splash animation (public/splash.html) runs.
const SPLASH_MS: u64 = 5100;

#[tauri::command]
fn app_ready() {
    UI_READY.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Lets the UI report JavaScript errors to the terminal for debugging.
#[tauri::command]
fn log_js(message: String) {
    eprintln!("[graphite][ui] {message}");
}

#[tauri::command]
async fn create_folder(name: String, parent_id: Option<String>, account: Option<String>) -> CmdResult<String> {
    folders::create(&name, parent_id.as_deref(), account.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn rename_folder(id: String, name: String) -> CmdResult<()> {
    folders::rename(&id, &name).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_folder(id: String) -> CmdResult<()> {
    folders::delete(&id).await.map_err(|e| e.to_string())
}

/// Background loop: sync every 2 minutes while signed in; notify the UI after each pass.
fn spawn_sync_loop(app: tauri::AppHandle) {
    use tauri::Emitter;
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            if auth::current_account().is_some() && storage::read_bootstrap().is_some() {
                let res = sync::sync_all().await.map_err(|e| e.to_string());
                match &res {
                    Ok(_) => notify::announce(&app),
                    Err(e) => eprintln!("[graphite] background sync failed: {e}"),
                }
                let _ = app.emit("sync-finished", res);
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            app_ready,
            list_accounts,
            save_draft,
            list_drafts,
            delete_draft,
            get_signature,
            set_signature,
            list_attachments,
            open_attachment,
            save_attachment,
            list_rules,
            save_rule,
            delete_rule,
            set_rule_enabled,
            run_rule,
            enable_rules,
            remove_account,
            add_google_account,
            add_imap_account,
            google_sign_in_available,
            contacts_overview,
            contact_detail,
            refresh_contacts,
            grant_contacts,
            remove_cached_contacts,
            import_contacts,
            suggest_recipients,
            set_contact_favourite,
            contact_details,
            export_vcards,
            block_address,
            cancel_sign_in,
            admin_consent_link,
            get_notifications,
            set_notifications,
            create_folder,
            rename_folder,
            delete_folder,
            sign_in,
            current_account,
            sign_out,
            check_session,
            setup_state,
            initialize_storage,
            sync_now,
            list_folders,
            list_messages,
            get_message,
            image_sender_allowed,
            set_image_sender,
            search_messages,
            set_read,
            set_flag,
            move_message,
            delete_message,
            copy_message,
            mark_spam,
            block_sender,
            get_theme,
            set_theme,
            send_mail,
            pending_actions,
            pending_details,
            data_folder_info,
            move_data_folder,
            retry_pending,
            discard_failed,
            list_calendars,
            list_events,
            rsvp_event,
            create_event,
            delete_event,
            open_link,
            log_js
        ])
        .setup(|app| {
            attachments::clear_temp();
            let handle = app.handle().clone();
            let handle2 = app.handle().clone();
            let cfg = app.config().app.windows[0].clone();
            // The main window loads in the background, hidden, while the splash animation plays.
            let main = tauri::WebviewWindowBuilder::from_config(app.handle(), &cfg)?
                .visible(false)
                // Developer tools only in debug builds (release builds also lack the "devtools" feature).
                .devtools(cfg!(debug_assertions))
                .on_navigation(move |url| allow_navigation(&handle, url))
                .on_new_window(move |url, _features| new_window_request(&handle2, url))
                .build()?;
            match tauri::WebviewWindowBuilder::new(app, "splash", tauri::WebviewUrl::App("splash.html".into()))
                // The splash only ever shows its own page: no links, no other pages, no windows.
                .devtools(cfg!(debug_assertions))
                .on_navigation(|url| links::classify(url, cfg!(dev)) == links::Action::Allow)
                .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
                .title("Graphite Mail")
                .inner_size(520.0, 520.0)
                .decorations(false)
                .resizable(false)
                .center()
                .always_on_top(true)
                .skip_taskbar(true)
                .build()
            {
                Ok(splash) => {
                    tauri::async_runtime::spawn(async move {
                        // Wait for the whole animation, and for the UI to be ready (but never longer than 12 s).
                        tokio::time::sleep(std::time::Duration::from_millis(SPLASH_MS)).await;
                        for _ in 0..180 {
                            if UI_READY.load(std::sync::atomic::Ordering::SeqCst) {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                        let _ = main.show();
                        let _ = main.set_focus();
                        let _ = splash.close();
                    });
                }
                Err(e) => {
                    eprintln!("[graphite] splash window failed: {e}");
                    let _ = main.show();
                }
            }
            spawn_sync_loop(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
