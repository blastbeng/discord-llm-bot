use std::env;

pub struct Lang {
    pub join_success: String,
    pub join_error: String,
    pub leave_success: String,
    pub not_connected: String,
    pub stop_success: String,
    pub tts_error: String,
    pub playing: String,
    pub no_sentence: String,
    pub invalid_extension: String,
    pub audio_playback: String,
    pub admin_parent_server: String,
    pub admin_only: String,
    pub restarting: String,
    pub nickname_too_long: String,
    pub nickname_changed: String,
    pub unsupported_file: String,
    pub avatar_changed: String,
    pub spam_detected: String,
}

impl Lang {
    pub fn new() -> Self {
        let lang = env::var("LANG").unwrap_or_else(|_| "ita".to_string());
        match lang.as_str() {
            "eng" => Self {
                join_success: "I'm joining the channel".to_string(),
                join_error: "Error joining the channel".to_string(),
                leave_success: "I'm leaving the channel".to_string(),
                not_connected: "I'm not connected to any channel".to_string(),
                stop_success: "Stopping the bot".to_string(),
                tts_error: "Error generating audio, please try again in a moment.".to_string(),
                playing: "Playing: **{}** with voice: {}".to_string(),
                no_sentence: "No sentence found".to_string(),
                invalid_extension: "The file extension is not valid.".to_string(),
                audio_playback: "Done! I'm starting the audio playback!".to_string(),
                admin_parent_server: "Only administrators can use this command in the parent server".to_string(),
                admin_only: "Only administrators can use this command".to_string(),
                restarting: "I'm restarting the bot.".to_string(),
                nickname_too_long: "My nickname cannot be longer than 32 characters".to_string(),
                nickname_changed: "You renamed me to \"{}\"".to_string(),
                unsupported_file: "This file type is not supported".to_string(),
                avatar_changed: "The image has been changed".to_string(),
                spam_detected: "Spam detected. <@{}> I'm watching you.\nCooldown: {}s".to_string(),
            },
            _ => Self {
                join_success: "Sto entrando nel canale".to_string(),
                join_error: "Errore nell'entrare nel canale".to_string(),
                leave_success: "Sto lasciando il canale".to_string(),
                not_connected: "Non sono connesso a nessun canale".to_string(),
                stop_success: "Interrompo il bot".to_string(),
                tts_error: "Errore nella generazione dell'audio, riprovare fra qualche istante.".to_string(),
                playing: "Sto riproducendo: **{}** con voce: {}".to_string(),
                no_sentence: "Nessuna frase trovata".to_string(),
                invalid_extension: "The file extension is not valid.".to_string(),
                audio_playback: "Done! I'm starting the audio playback!".to_string(),
                admin_parent_server: "Solo gli amministratori possono utilizzare questo comando nel server padre".to_string(),
                admin_only: "Solo gli amministratori possono utilizzare questo comando".to_string(),
                restarting: "Sto riavviando il bot.".to_string(),
                nickname_too_long: "Il mio nickname non puó essere piú lungo di 32 caratteri".to_string(),
                nickname_changed: "Mi hai rinominato in \"{}\"".to_string(),
                unsupported_file: "Questo tipo di file non é supportato".to_string(),
                avatar_changed: "L'immagine é stata modificata".to_string(),
                spam_detected: "Spam detected. <@{}> Ti sto guardando.\nCooldown: {}s".to_string(),
            }
        }
    }
}
