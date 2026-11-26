use super::*;
use std::sync::Mutex;
use tempfile::TempDir;
use voice_core::config::XaiSettings;
use wiremock::{
    matchers::{body_partial_json, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ok,
    MissingKey,
    Api(u16),
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Captured {
    text: String,
    voice_id: String,
    language: String,
    speed: Option<f64>,
    text_normalization: bool,
    fields: Vec<(String, String)>,
    filename: String,
    content_type: String,
}

struct FakeXai {
    mode: Mode,
    captured: Mutex<Captured>,
    created: Value,
}

impl FakeXai {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            captured: Mutex::new(Captured::default()),
            created: json!({"voice_id": "clone123"}),
        })
    }

    fn fail(&self) -> Result<(), XaiError> {
        match self.mode {
            Mode::Ok => Ok(()),
            Mode::MissingKey => Err(XaiError::MissingKey),
            Mode::Api(status) => Err(XaiError::Api {
                status,
                message: "nope".into(),
            }),
        }
    }

    fn captured(&self) -> Captured {
        self.captured.lock().unwrap().clone()
    }
}

#[async_trait]
impl BrandXai for FakeXai {
    async fn synthesize_wav(&self, request: &TtsRequest<'_>) -> Result<Vec<u8>, XaiError> {
        self.fail()?;
        let mut captured = self.captured.lock().unwrap();
        captured.text = request.text.to_string();
        captured.voice_id = request.voice_id.to_string();
        captured.language = request.language.to_string();
        captured.speed = request.speed;
        captured.text_normalization = request.text_normalization;
        Ok(b"RIFF".to_vec())
    }
    async fn list_tts_voices(&self) -> Result<Vec<Value>, XaiError> {
        self.fail()?;
        Ok(vec![
            json!({"voice_id": "eve", "name": "Eve", "tone": "bright"}),
            json!({"name": "no id"}),
            json!("junk"),
        ])
    }
    async fn list_custom_voices(&self) -> Result<Vec<Value>, XaiError> {
        self.fail()?;
        Ok(vec![
            json!({"id": "Owner1", "language": "en-AU"}),
            json!({"voice_id": "EVE"}),
        ])
    }
    async fn create_custom_voice(
        &self,
        fields: &[(&str, &str)],
        _audio: Vec<u8>,
        filename: &str,
        content_type: &str,
    ) -> Result<Value, XaiError> {
        self.fail()?;
        let mut captured = self.captured.lock().unwrap();
        captured.fields = fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        captured.filename = filename.to_string();
        captured.content_type = content_type.to_string();
        Ok(self.created.clone())
    }
}

fn write_pack(root: &Path, client_id: &str, company: &str, greeting: &str) {
    let folder = root.join(client_id);
    std::fs::create_dir_all(&folder).unwrap();
    let greeting = if greeting.is_empty() {
        format!("Hello from {company}")
    } else {
        greeting.to_string()
    };
    let payload = json!({
        "id": client_id,
        "display_name": company,
        "brand": {"company": company, "greeting": greeting, "tts_voice": "eve", "language": "en"},
    });
    std::fs::write(folder.join("client.json"), payload.to_string()).unwrap();
}

struct Fixture {
    tmp: TempDir,
    store: BrandStore,
}

impl Fixture {
    fn brand_dir(&self) -> PathBuf {
        self.tmp.path().join("brand")
    }
    fn clients_dir(&self) -> PathBuf {
        self.tmp.path().join("clients")
    }
    fn messages_dir(&self) -> PathBuf {
        self.brand_dir().join("acme").join("messages")
    }
    fn message_files(&self) -> Vec<String> {
        std::fs::read_dir(self.messages_dir())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn setup_with(xai: Arc<dyn BrandXai>) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let clients_dir = tmp.path().join("clients");
    write_pack(&clients_dir, "acme", "Acme Pty Ltd", "Good morning, Acme.");
    write_pack(&clients_dir, "_template", "Template Co", "");
    let settings = BrandSettings {
        brand_dir: tmp.path().join("brand"),
        clients_dir,
        default_client_id: "acme".to_string(),
    };
    Fixture {
        store: BrandStore::new(settings, xai),
        tmp,
    }
}

fn setup() -> Fixture {
    setup_with(FakeXai::new(Mode::Ok))
}

fn synth_request() -> SynthesizeRequest {
    SynthesizeRequest {
        title: "EAC Welcome Message".into(),
        kind: "greeting".into(),
        script: "Thank you for calling Exclusive Auto Center.".into(),
        voice_id: "ara".into(),
        voice_name: "Ara".into(),
        language: "en-GB".into(),
        speed: 0.9,
        ..Default::default()
    }
}

fn clone_request() -> CloneRequest {
    CloneRequest {
        name: "Owner".into(),
        filename: "owner.WAV".into(),
        content_type: "audio/wav".into(),
        bytes: b"RIFF".to_vec(),
        ..Default::default()
    }
}

fn assert_err<T: std::fmt::Debug>(
    result: Result<T, BrandError>,
    status: u16,
    code: &str,
    message: &str,
) {
    let err = result.unwrap_err();
    assert_eq!(
        (err.status_code, err.code.as_str(), err.message.as_str()),
        (status, code, message)
    );
}

// ---- 1:1 ports of tests/test_brand.py -------------------------------------

#[test]
fn test_list_clients_skips_template() {
    let fx = setup();
    let clients = fx.store.list_clients();
    assert_eq!(
        clients.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["acme"]
    );
    assert_eq!(clients[0].company, "Acme Pty Ltd");
}

#[test]
fn test_closed_template_uses_dates() {
    let text = script_template("closed", "Acme Pty Ltd", "2026-12-24", "2027-01-02", "");
    assert!(text.contains("Acme Pty Ltd"));
    assert!(text.contains("24 December 2026"));
    assert!(text.contains("2 January 2027"));
}

#[test]
fn test_greeting_prefers_pack_copy() {
    let text = script_template("greeting", "Acme Pty Ltd", "", "", "Good morning, Acme.");
    assert_eq!(text, "Good morning, Acme.");
}

#[test]
fn test_require_id_rejects_paths() {
    let fx = setup();
    assert!(require_id("../secret", "id").is_err());
    assert!(fx.store.get_client("missing").is_err());
}

#[test]
fn test_register_voice_and_message_library() {
    let fx = setup();
    let voice = fx
        .store
        .register_cloned_voice("acme", "nlbqfwie", "Owner", "", "")
        .unwrap();
    assert_eq!(voice.kind, "cloned");
    let catalog: Value = serde_json::from_str(
        &std::fs::read_to_string(fx.brand_dir().join("acme").join("voices.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(catalog["voices"][0]["voice_id"], "nlbqfwie");

    let message: BrandMessage = serde_json::from_value(json!({
        "id": "abc123def456",
        "client_id": "acme",
        "title": "Christmas close",
        "kind": "closed",
        "script": "We are closed.",
        "voice_id": "eve",
        "starts_on": "2026-12-24",
        "ends_on": "2027-01-02",
    }))
    .unwrap();
    let folder = fx.messages_dir();
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join(format!("{}.json", message.id)),
        serde_json::to_string(&message).unwrap(),
    )
    .unwrap();
    std::fs::write(folder.join(format!("{}.wav", message.id)), b"RIFF").unwrap();
    let listed = fx.store.list_messages("acme").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        download_filename(&listed[0]),
        "acme-closed-christmas-close.wav"
    );
    fx.store.delete_message("acme", &message.id).unwrap();
    assert_eq!(fx.store.list_messages("acme").unwrap(), vec![]);
    assert!(!folder.join(format!("{}.wav", message.id)).exists());
}

#[test]
fn test_normalize_speed_bounds() {
    assert_eq!(normalize_speed(None).unwrap(), 1.0);
    assert_eq!(normalize_speed(Some(&json!("0.85"))).unwrap(), 0.85);
    assert_eq!(normalize_speed(Some(&json!(1.5))).unwrap(), 1.5);
    assert!(normalize_speed(Some(&json!(0.5))).is_err());
    assert!(normalize_speed(Some(&json!(2))).is_err());
    assert!(normalize_speed(Some(&json!("fast"))).is_err());
}

#[test]
fn test_normalize_language_accents() {
    assert_eq!(normalize_language("", "en").unwrap(), "en");
    assert_eq!(normalize_language("en-gb", "en").unwrap(), "en-GB");
    assert_eq!(normalize_language("EN-AU", "en").unwrap(), "en-AU");
    assert_eq!(language_label("en-GB"), "English — UK");
    assert!(normalize_language("en-XX", "en").is_err());
}

#[tokio::test]
async fn test_synthesize_passes_speed_and_accent() {
    let xai = FakeXai::new(Mode::Ok);
    let fx = setup_with(xai.clone());
    let message = fx
        .store
        .synthesize_message("acme", synth_request())
        .await
        .unwrap();
    assert_eq!(message.language, "en-GB");
    assert_eq!(message.speed, 0.9);
    let captured = xai.captured();
    assert_eq!(captured.language, "en-GB");
    assert_eq!(captured.speed, Some(0.9));
    assert_eq!(captured.voice_id, "ara");
}

// ---- Additional coverage ---------------------------------------------------

#[tokio::test]
async fn synthesize_saves_message_and_wav() {
    let xai = FakeXai::new(Mode::Ok);
    let fx = setup_with(xai.clone());
    let message = fx
        .store
        .synthesize_message("acme", synth_request())
        .await
        .unwrap();
    assert_eq!(message.id.len(), 12);
    assert!(message.id.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(message.audio_bytes, 4);
    assert_eq!(
        message.download_name,
        "acme-greeting-eac-welcome-message.wav"
    );
    assert_eq!(message.voice_name, "Ara");
    assert!(message.created_at.ends_with("+00:00"));
    assert_eq!(message.created_at, message.updated_at);
    assert!(xai.captured().text_normalization);

    let wav = fx.store.message_audio_path("acme", &message.id).unwrap();
    assert_eq!(wav, fx.messages_dir().join(format!("{}.wav", message.id)));
    assert_eq!(std::fs::read(&wav).unwrap(), b"RIFF");
    assert_eq!(fx.store.get_message("acme", &message.id).unwrap(), message);
    assert_eq!(
        fx.store.list_messages("acme").unwrap(),
        vec![message.clone()]
    );

    // Stored like Python's json.dumps(indent=2), fields in model order.
    let text =
        std::fs::read_to_string(fx.messages_dir().join(format!("{}.json", message.id))).unwrap();
    assert!(text.starts_with(&format!(
        "{{\n  \"id\": \"{}\",\n  \"client_id\": \"acme\",\n",
        message.id
    )));
    assert!(text.contains("\n  \"speed\": 0.9,\n"));
    assert!(text.ends_with("\n}"));
}

#[tokio::test]
async fn synthesize_defaults_title_voice_and_language() {
    let fx = setup();
    let request = SynthesizeRequest {
        kind: " Closed ".into(),
        script: "  We are closed.  ".into(),
        starts_on: "2026-12-24T00:00".into(),
        ends_on: "2027-01-02".into(),
        ..Default::default()
    };
    let message = fx.store.synthesize_message("acme", request).await.unwrap();
    assert_eq!(message.title, "Closed period 2026-12-24 to 2027-01-02");
    assert_eq!(message.kind, "closed");
    assert_eq!(message.script, "We are closed.");
    assert_eq!(
        (message.voice_id.as_str(), message.voice_name.as_str()),
        ("eve", "eve")
    );
    assert_eq!(message.language, "en");
    assert_eq!(message.speed, 1.0);
    assert_eq!(message.starts_on, "2026-12-24");
}

#[tokio::test]
async fn synthesize_validation_errors() {
    let fx = setup();
    let store = &fx.store;
    let with = |edit: fn(&mut SynthesizeRequest)| {
        let mut request = synth_request();
        edit(&mut request);
        request
    };

    assert_err(
        store.synthesize_message("../x", synth_request()).await,
        400,
        "brand_error",
        "Invalid client id",
    );
    assert_err(
        store.synthesize_message("missing", synth_request()).await,
        404,
        "unknown_client",
        "Unknown client 'missing'",
    );
    assert_err(
        store
            .synthesize_message("acme", with(|r| r.script = "   ".into()))
            .await,
        400,
        "brand_error",
        "Write the message script first",
    );
    assert_err(
        store
            .synthesize_message("acme", with(|r| r.script = "a".repeat(15001)))
            .await,
        400,
        "brand_error",
        "Script is too long (max 15,000 characters)",
    );
    assert_err(
        store
            .synthesize_message("acme", with(|r| r.voice_id = "   ".into()))
            .await,
        400,
        "brand_error",
        "Choose a voice",
    );
    assert_err(
        store
            .synthesize_message("acme", with(|r| r.kind = "birthday".into()))
            .await,
        400,
        "brand_error",
        "Unknown message type",
    );
    assert_err(
        store
            .synthesize_message("acme", with(|r| r.starts_on = "24/12/2026".into()))
            .await,
        400,
        "brand_error",
        "Dates must be YYYY-MM-DD",
    );
    assert_err(
        store
            .synthesize_message("acme", with(|r| r.ends_on = "2026-02-30".into()))
            .await,
        400,
        "brand_error",
        "Dates must be YYYY-MM-DD",
    );
    assert_err(
        store
            .synthesize_message(
                "acme",
                with(|r| {
                    r.starts_on = "2027-01-02".into();
                    r.ends_on = "2026-12-24".into();
                }),
            )
            .await,
        400,
        "brand_error",
        "Closed-from date must be before closed-until",
    );
    assert_err(
        store
            .synthesize_message("acme", with(|r| r.language = "en-XX".into()))
            .await,
        400,
        "brand_error",
        "Unknown accent/language 'en-XX'",
    );
    assert_err(
        store
            .synthesize_message("acme", with(|r| r.speed = 2.0))
            .await,
        400,
        "brand_error",
        "Speed must be between 0.7 and 1.5",
    );
    assert_err(
        store
            .synthesize_message("acme", with(|r| r.speed = f64::NAN))
            .await,
        400,
        "brand_error",
        "Speed must be a number",
    );
    assert!(fx.message_files().is_empty());
}

#[test]
fn speed_and_language_error_details() {
    assert_err(
        normalize_speed(Some(&json!("fast"))),
        400,
        "brand_error",
        "Speed must be a number",
    );
    assert_err(
        normalize_speed(Some(&json!(true))),
        400,
        "brand_error",
        "Speed must be a number",
    );
    assert_err(
        normalize_speed(Some(&json!(0.5))),
        400,
        "brand_error",
        "Speed must be between 0.7 and 1.5",
    );
    assert_eq!(normalize_speed(Some(&json!(""))).unwrap(), 1.0);
    assert_eq!(normalize_speed(Some(&Value::Null)).unwrap(), 1.0);
    assert_eq!(normalize_speed(Some(&json!(" 0.849 "))).unwrap(), 0.85);
    assert_eq!(normalize_speed(Some(&json!(1))).unwrap(), 1.0);
    assert_err(
        normalize_language("", "klingon"),
        400,
        "brand_error",
        "Unknown accent/language 'klingon'",
    );
    assert_eq!(normalize_language("  ", "  ").unwrap(), "en");
    assert_eq!(language_label("xx"), "xx");
    assert_eq!(language_label(""), "English");
}

#[test]
fn id_and_lookup_errors() {
    let fx = setup();
    assert_err(
        require_id("", "voice id"),
        400,
        "brand_error",
        "Invalid voice id",
    );
    assert_err(
        require_id(&"a".repeat(65), "id"),
        400,
        "brand_error",
        "Invalid id",
    );
    assert_eq!(require_id("  a.b_c-1  ", "id").unwrap(), "a.b_c-1");
    assert_err(
        fx.store.get_client("_template"),
        400,
        "brand_error",
        "Invalid client id",
    );
    assert_err(
        fx.store.list_messages("missing"),
        404,
        "unknown_client",
        "Unknown client 'missing'",
    );
    assert_err(
        fx.store.get_message("acme", "abc123"),
        404,
        "unknown_message",
        "Message not found",
    );
    assert_err(
        fx.store.get_message("acme", "../abc"),
        400,
        "brand_error",
        "Invalid message id",
    );
    assert_err(
        fx.store.message_audio_path("acme", "abc123"),
        404,
        "unknown_message",
        "Message not found",
    );
    assert_err(
        fx.store.delete_message("acme", "abc123"),
        404,
        "unknown_message",
        "Message not found",
    );
    assert_err(
        fx.store
            .register_cloned_voice("acme", "bad id!", "", "", ""),
        400,
        "brand_error",
        "Invalid voice id",
    );
}

#[tokio::test]
async fn clone_validation_errors() {
    let xai = FakeXai::new(Mode::Ok);
    let fx = setup_with(xai.clone());
    let store = &fx.store;
    let with = |edit: fn(&mut CloneRequest)| {
        let mut request = clone_request();
        edit(&mut request);
        request
    };
    assert_err(
        store
            .clone_voice_from_sample("missing", clone_request())
            .await,
        404,
        "unknown_client",
        "Unknown client 'missing'",
    );
    assert_err(
        store
            .clone_voice_from_sample("acme", with(|r| r.name = "  ".into()))
            .await,
        400,
        "brand_error",
        "Give the cloned voice a name",
    );
    assert_err(
        store
            .clone_voice_from_sample("acme", with(|r| r.bytes.clear()))
            .await,
        400,
        "brand_error",
        "Upload a voice sample",
    );
    assert_err(
        store
            .clone_voice_from_sample("acme", with(|r| r.bytes = vec![0; 25 * 1024 * 1024 + 1]))
            .await,
        400,
        "brand_error",
        "Sample is too large (max 25 MB)",
    );
    assert_err(
        store
            .clone_voice_from_sample("acme", with(|r| r.filename = "notes.txt".into()))
            .await,
        400,
        "brand_error",
        "Use a WAV, MP3, M4A, FLAC, or OGG sample",
    );
    assert_eq!(xai.captured(), Captured::default());
}

#[tokio::test]
async fn clone_registers_voice_in_catalog() {
    let xai = FakeXai::new(Mode::Ok);
    let fx = setup_with(xai.clone());
    let request = CloneRequest {
        gender: "female".into(),
        ..clone_request()
    };
    let voice = fx
        .store
        .clone_voice_from_sample("acme", request)
        .await
        .unwrap();
    assert_eq!(
        voice,
        BrandVoice {
            voice_id: "clone123".into(),
            name: "Owner".into(),
            kind: "cloned".into(),
            description: String::new(),
            language: "en".into(),
            source: "catalog".into(),
        }
    );
    let captured = xai.captured();
    let pairs: Vec<(&str, &str)> = captured
        .fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("name", "Owner"),
            ("language", "en"),
            ("use_case", "narration"),
            ("gender", "female"),
            ("tone", ""),
            ("description", "Brand voice for Acme Pty Ltd"),
        ]
    );
    assert_eq!(
        (captured.filename.as_str(), captured.content_type.as_str()),
        ("owner.WAV", "audio/wav")
    );
    let listed = fx.store.list_voices("acme").await.unwrap();
    assert!(listed
        .cloned
        .iter()
        .any(|v| v.voice_id == "clone123" && v.source == "catalog"));
}

#[tokio::test]
async fn clone_403_is_clone_api_gated() {
    let fx = setup_with(FakeXai::new(Mode::Api(403)));
    assert_err(
        fx.store
            .clone_voice_from_sample("acme", clone_request())
            .await,
        403,
        "clone_api_gated",
        "xAI clone-from-API is not enabled on this team (Enterprise). \
         Clone the sample in the xAI voice library, then paste the Voice ID here. (nope)",
    );
    assert!(!fx.brand_dir().join("acme").join("voices.json").exists());
}

#[tokio::test]
async fn clone_other_failures() {
    let fx = setup_with(FakeXai::new(Mode::Api(500)));
    assert_err(
        fx.store
            .clone_voice_from_sample("acme", clone_request())
            .await,
        502,
        "clone_failed",
        "xAI 500: nope",
    );

    let xai = Arc::new(FakeXai {
        mode: Mode::Ok,
        captured: Mutex::default(),
        created: json!({"name": "x"}),
    });
    let fx = setup_with(xai);
    assert_err(
        fx.store
            .clone_voice_from_sample("acme", clone_request())
            .await,
        502,
        "brand_error",
        "Clone succeeded but no voice_id was returned",
    );
}

#[tokio::test]
async fn missing_key_is_xai_unavailable() {
    let fx = setup_with(FakeXai::new(Mode::MissingKey));
    let message = "XAI_API_KEY is missing. Add it to your local environment or .env file.";
    assert_err(
        fx.store.synthesize_message("acme", synth_request()).await,
        503,
        "xai_unavailable",
        message,
    );
    assert_err(
        fx.store
            .clone_voice_from_sample("acme", clone_request())
            .await,
        503,
        "xai_unavailable",
        message,
    );
    assert!(fx.message_files().is_empty());
}

#[tokio::test]
async fn tts_failure_leaves_no_files() {
    let fx = setup_with(FakeXai::new(Mode::Api(500)));
    assert_err(
        fx.store.synthesize_message("acme", synth_request()).await,
        502,
        "tts_failed",
        "xAI 500: nope",
    );
    assert!(fx.message_files().is_empty());
    assert_eq!(fx.store.list_messages("acme").unwrap(), vec![]);
}

#[tokio::test]
async fn voice_listing_falls_back_when_xai_fails() {
    let fx = setup_with(FakeXai::new(Mode::Api(500)));
    fx.store
        .register_cloned_voice("acme", "Owner1", "Owner", " The boss ", "en-AU")
        .unwrap();
    fx.store
        .register_cloned_voice("acme", "EVE", "Shadows a stock voice", "", "")
        .unwrap();
    let listed = fx.store.list_voices("acme").await.unwrap();
    assert_eq!(listed.xai_error, "xAI 500: nope");
    assert_eq!(listed.client.id, "acme");
    assert_eq!(listed.pack_voice_id, "eve");
    assert_eq!(
        listed
            .stock
            .iter()
            .map(|v| v.voice_id.as_str())
            .collect::<Vec<_>>(),
        ["eve", "ara", "rex", "leo", "sal"]
    );
    assert!(listed
        .stock
        .iter()
        .all(|v| v.source == "fallback" && v.kind == "stock" && v.language == "en"));
    assert_eq!(listed.stock[0].description, "Default brand voice");
    assert_eq!(
        listed.cloned,
        vec![BrandVoice {
            voice_id: "Owner1".into(),
            name: "Owner".into(),
            kind: "cloned".into(),
            description: "The boss".into(),
            language: "en-AU".into(),
            source: "catalog".into(),
        }]
    );

    let fx = setup_with(FakeXai::new(Mode::MissingKey));
    let listed = fx.store.list_voices("acme").await.unwrap();
    assert!(listed.xai_error.starts_with("XAI_API_KEY is missing"));
    assert_eq!(listed.stock.len(), 5);
    assert_err(
        fx.store.list_voices("missing").await,
        404,
        "unknown_client",
        "Unknown client 'missing'",
    );
}

#[tokio::test]
async fn voice_listing_merges_xai_and_catalog() {
    let fx = setup();
    fx.store
        .register_cloned_voice("acme", "owner1", "Duplicate of xAI voice", "", "")
        .unwrap();
    fx.store
        .register_cloned_voice("acme", "extra9", "", "", "")
        .unwrap();
    let listed = fx.store.list_voices("acme").await.unwrap();
    assert_eq!(listed.xai_error, "");
    assert_eq!(listed.stock.len(), 1);
    assert_eq!(
        (
            listed.stock[0].voice_id.as_str(),
            listed.stock[0].description.as_str(),
            listed.stock[0].source.as_str()
        ),
        ("eve", "bright", "xai")
    );
    let cloned: Vec<(&str, &str, &str, &str)> = listed
        .cloned
        .iter()
        .map(|v| {
            (
                v.voice_id.as_str(),
                v.name.as_str(),
                v.language.as_str(),
                v.source.as_str(),
            )
        })
        .collect();
    assert_eq!(
        cloned,
        [
            ("Owner1", "Owner1", "en-AU", "xai"),
            ("extra9", "extra9", "en", "catalog")
        ]
    );
}

#[test]
fn register_replaces_existing_entry_and_writes_python_style_json() {
    let fx = setup();
    fx.store
        .register_cloned_voice("acme", "Abc", "First", "", "")
        .unwrap();
    fx.store
        .register_cloned_voice("acme", "abc", "Señor", "", "es-MX")
        .unwrap();
    let text = std::fs::read_to_string(fx.brand_dir().join("acme").join("voices.json")).unwrap();
    assert!(text.starts_with(
        "{\n  \"voices\": [\n    {\n      \"voice_id\": \"abc\",\n      \"name\": \"Se\\u00f1or\","
    ));
    let catalog: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(catalog["voices"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["voices"][0]["name"], "Señor");
    assert_eq!(catalog["voices"][0]["language"], "es-MX");
    assert_eq!(catalog["voices"][0]["source"], "imported");
    assert!(catalog["voices"][0]["created_at"]
        .as_str()
        .unwrap()
        .ends_with("+00:00"));
}

#[test]
fn bad_files_on_disk_are_skipped() {
    let fx = setup();
    let clients = fx.clients_dir();
    for (name, body) in [
        ("broken", "{not json"),
        ("listy", "[1, 2]"),
        ("bad id", "{\"id\": \"no/slash\"}"),
    ] {
        std::fs::create_dir_all(clients.join(name)).unwrap();
        std::fs::write(clients.join(name).join("client.json"), body).unwrap();
    }
    std::fs::create_dir_all(clients.join("empty")).unwrap();
    std::fs::create_dir_all(clients.join("Bare")).unwrap();
    std::fs::write(clients.join("Bare").join("client.json"), "{}").unwrap();
    let clients = fx.store.list_clients();
    assert_eq!(
        clients.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["acme", "Bare"]
    );
    assert_eq!(
        clients[1],
        BrandClient {
            id: "Bare".into(),
            display_name: "Bare".into(),
            company: "Bare".into(),
            tts_voice: "eve".into(),
            language: "en".into(),
            greeting: String::new(),
        }
    );

    let folder = fx.messages_dir();
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("broken.json"), "{oops").unwrap();
    std::fs::write(folder.join("partial.json"), "{\"id\": \"partial\"}").unwrap();
    std::fs::write(fx.brand_dir().join("acme").join("voices.json"), "nonsense").unwrap();
    assert_eq!(fx.store.list_messages("acme").unwrap(), vec![]);
    assert_eq!(
        fx.store
            .get_message("acme", "broken")
            .unwrap_err()
            .status_code,
        500
    );
    fx.store
        .register_cloned_voice("acme", "v1", "", "", "")
        .unwrap();
}

#[test]
fn messages_sort_newest_first() {
    let fx = setup();
    let folder = fx.messages_dir();
    std::fs::create_dir_all(&folder).unwrap();
    for (id, created, updated) in [
        ("a1", "2026-01-01", ""),
        ("b2", "2026-01-03", ""),
        ("c3", "2025-01-01", "2026-01-02"),
    ] {
        let body = json!({"id": id, "client_id": "acme", "title": id, "script": "s", "voice_id": "eve",
            "created_at": created, "updated_at": updated});
        std::fs::write(folder.join(format!("{id}.json")), body.to_string()).unwrap();
    }
    let ids: Vec<String> = fx
        .store
        .list_messages("acme")
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, ["b2", "c3", "a1"]);
}

#[test]
fn templates_and_titles() {
    let fx = setup();
    let client = fx.store.get_client("acme").unwrap();
    assert_eq!(
        fx.store.script_template(&client, "greeting", "", ""),
        "Good morning, Acme."
    );
    assert_eq!(
        fx.store.script_template(&client, "after_hours", "", ""),
        "Thank you for calling Acme Pty Ltd. Our office is closed for the day. \
         Please call back during business hours, or leave a message after the tone."
    );
    assert_eq!(
        script_template("closed", "", "", "", ""),
        "Thank you for calling us. We are closed for a short period. \
         Please call back when we reopen, or leave a message after the tone."
    );
    assert_eq!(
        script_template("HOLIDAY", "Acme", "2026-12-24", "soon", ""),
        "Thank you for calling Acme. We are closed for the holiday period \
         from 24 December 2026 to soon. We look forward to speaking with you when we reopen."
    );
    assert_eq!(
        script_template("holiday", "Acme", "", "", ""),
        "Thank you for calling Acme. We are closed for the holiday period. Please call back when we reopen."
    );
    assert_eq!(
        script_template("greeting", "Acme", "", "", " "),
        "Good morning, thank you for calling Acme. How can I help you today?"
    );
    assert_eq!(script_template("custom", "Acme", "", "", ""), "");
    assert_eq!(
        template_title("Closed", "2026-12-24", "2027-01-02"),
        "Closed period 2026-12-24 to 2027-01-02"
    );
    assert_eq!(template_title("nope", "", ""), "Brand message");
    assert_eq!(kind_label("after_hours"), Some("After hours"));
}

#[test]
fn download_filename_and_default_client() {
    let mut message: BrandMessage = serde_json::from_value(
        json!({"id": "m1", "client_id": "acme", "title": "", "script": "s", "voice_id": "eve"}),
    )
    .unwrap();
    assert_eq!(
        (
            message.kind.as_str(),
            message.language.as_str(),
            message.speed
        ),
        ("custom", "en", 1.0)
    );
    assert_eq!(download_filename(&message), "acme-custom-custom.wav");
    message.title = "!!!".into();
    assert_eq!(download_filename(&message), "acme-custom-message.wav");
    message.title = "x".repeat(60);
    assert_eq!(
        download_filename(&message),
        format!("acme-custom-{}.wav", "x".repeat(48))
    );
    message.download_name = "kept.wav".into();
    assert_eq!(download_filename(&message), "kept.wav");

    let fx = setup();
    if std::env::var("VOICE_AI_DEFAULT_CLIENT_ID").is_err() {
        assert_eq!(fx.store.default_client_id(), "acme");
        let settings = BrandSettings {
            brand_dir: fx.brand_dir(),
            clients_dir: fx.clients_dir(),
            default_client_id: "gone".into(),
        };
        assert_eq!(
            BrandStore::new(settings, FakeXai::new(Mode::Ok)).default_client_id(),
            "acme"
        );
        let settings = BrandSettings {
            brand_dir: fx.brand_dir(),
            clients_dir: fx.tmp.path().join("nowhere"),
            default_client_id: "gone".into(),
        };
        assert_eq!(
            BrandStore::new(settings, FakeXai::new(Mode::Ok)).default_client_id(),
            "gone"
        );
    }
}

#[tokio::test]
async fn real_xai_rest_synthesizes_wav_over_http() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/tts"))
        .and(header("authorization", "Bearer k"))
        .and(body_partial_json(json!({
            "text": "Thank you for calling Exclusive Auto Center.",
            "voice_id": "ara",
            "language": "en-GB",
            "speed": 0.9,
            "text_normalization": true,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"RIFFwavdata".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let rest = |key: &str| {
        XaiRest::new(XaiSettings {
            api_key: key.into(),
            base_url: server.uri(),
            realtime_url: String::new(),
            grok_model: "grok-test".into(),
            tts_voice: "eve".into(),
            language: "en".into(),
            stt_keyterm: None,
        })
    };
    let fx = setup_with(Arc::new(rest("k")));
    let message = fx
        .store
        .synthesize_message("acme", synth_request())
        .await
        .unwrap();
    assert_eq!(message.audio_bytes, 11);
    let wav = fx.store.message_audio_path("acme", &message.id).unwrap();
    assert_eq!(std::fs::read(wav).unwrap(), b"RIFFwavdata");

    // No key: fails before any request, as 503 xai_unavailable.
    let fx = setup_with(Arc::new(rest("")));
    let err = fx
        .store
        .synthesize_message("acme", synth_request())
        .await
        .unwrap_err();
    assert_eq!(
        (err.status_code, err.code.as_str()),
        (503, "xai_unavailable")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
