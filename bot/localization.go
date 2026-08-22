package main

import (
	"fmt"
	"os"
	"strings"
)

var translations = map[string]map[string]string{
	"spam_detected_1": {
		"ita": "Spam detected. %s Ti sto guardando.\n%s",
		"eng": "Spam detected. %s I'm watching you.\n%s",
	},
	"spam_detected_2": {
		"ita": "Spam detected. %s Questo non ti rende una brava persona.\n%s",
		"eng": "Spam detected. %s This doesn't make you a good person.\n%s",
	},
	"spam_detected_3": {
		"ita": "Spam detected. %s Sono stupido ma non noioso.\n%s",
		"eng": "Spam detected. %s I'm stupid but not boring.\n%s",
	},
	"spam_detected_4": {
		"ita": "Spam detected. %s Prenditi il tuo tempo.\n%s",
		"eng": "Spam detected. %s Take your time.\n%s",
	},
	"spam_detected_5": {
		"ita": "Spam detected. %s Mantieni la calma.\n%s",
		"eng": "Spam detected. %s Keep calm.\n%s",
	},
	"spam_detected_6": {
		"ita": "Spam detected. %s Anche a casa tua ti comporti cosí?\n%s",
		"eng": "Spam detected. %s Do you behave like this even at your house?\n%s",
	},
	"spam_detected_7": {
		"ita": "Spam detected. %s Perché sei cosí ansioso?\n%s",
		"eng": "Spam detected. %s Why are you so anxious?\n%s",
	},
	"spam_detected_8": {
		"ita": "Spam detected. %s Ti aggiungo alla blacklist.\n%s",
		"eng": "Spam detected. %s I'm adding you to the blacklist.\n%s",
	},
	"must_be_in_voice": {
		"ita": "Devi essere connesso a un canale vocale per utilizzare questo comando",
		"eng": "You must be connected to a voice channel to use this command",
	},
	"error_connecting": {
		"ita": "Errore durante la connessione al canale vocale",
		"eng": "Error connecting to voice channel",
	},
	"joining_channel": {
		"ita": "Sto entrando nel canale",
		"eng": "Joining the channel",
	},
	"not_connected": {
		"ita": "Non sono connesso a nessun canale",
		"eng": "I'm not connected to any channel",
	},
	"leaving_channel": {
		"ita": "Sto lasciando il canale",
		"eng": "Leaving the channel",
	},
	"stopping_bot": {
		"ita": "Interrompo il bot",
		"eng": "Stopping the bot",
	},
	"generating_audio": {
		"ita": "Inizio a generare l'audio per la frase: **%s**%s",
		"eng": "Starting to generate audio for the sentence: **%s**%s",
	},
	"playing_audio": {
		"ita": "Sto riproducendo: %s\nVoce: %s",
		"eng": "Playing: %s\nVoice: %s",
	},
	"fakeyou_fallback": {
		"ita": "Sto riproducendo: %s\nVoce: %s\n\nWARNING: FakeYou sta ricevendo troppe richieste, audio generato usando la voce di Google",
		"eng": "Playing: %s\nVoice: %s\n\nWARNING: FakeYou is receiving too many requests, audio generated using Google voice",
	},
	"searching_random": {
		"ita": "Sto cercando una frase casuale%s",
		"eng": "Searching for a random sentence%s",
	},
	"no_sentence_found": {
		"ita": "Nessuna frase trovata",
		"eng": "No sentence found",
	},
	"no_permissions": {
		"ita": "Non hai i permessi per utilizzare questo comando.",
		"eng": "You don't have permissions to use this command.",
	},
	"admin_only": {
		"ita": "Solo gli amministratori possono utilizzare questo comando",
		"eng": "Only administrators can use this command",
	},
	"restarting_bot": {
		"ita": "Sto riavviando il bot.",
		"eng": "Restarting the bot.",
	},
	"nick_too_long": {
		"ita": "Il mio nickname non può essere più lungo di 32 caratteri",
		"eng": "My nickname cannot be longer than 32 characters",
	},
	"error_nick": {
		"ita": "Errore durante il cambio di nickname.",
		"eng": "Error changing nickname.",
	},
	"nick_changed": {
		"ita": "Mi hai rinominato in \"%s\"",
		"eng": "You renamed me to \"%s\"",
	},
	"admin_only_parent": {
		"ita": "Solo gli amministratori possono utilizzare questo comando nel server padre",
		"eng": "Only administrators can use this command in the parent server",
	},
	"error_attachment": {
		"ita": "Errore durante il recupero dell'allegato.",
		"eng": "Error retrieving attachment.",
	},
	"unsupported_file": {
		"ita": "Questo tipo di file non è supportato",
		"eng": "This file type is not supported",
	},
	"error_download_image": {
		"ita": "Errore durante il download dell'immagine.",
		"eng": "Error downloading image.",
	},
	"error_read_image": {
		"ita": "Errore durante la lettura dell'immagine.",
		"eng": "Error reading image.",
	},
	"error_update_avatar": {
		"ita": "Errore durante l'aggiornamento dell'avatar.",
		"eng": "Error updating avatar.",
	},
	"avatar_changed": {
		"ita": "L'immagine è stata modificata",
		"eng": "The image has been changed",
	},
	"button_play": {
		"ita": "Play",
		"eng": "Play",
	},
	"button_stop": {
		"ita": "Stop",
		"eng": "Stop",
	},
	"invalid_audio_ext": {
		"ita": "L'estensione del file non è valida. Solo mp3 o wav sono permessi.",
		"eng": "The file extension is not valid. Only mp3 or wav are allowed.",
	},
	"error_download_audio": {
		"ita": "Errore durante il download dell'audio.",
		"eng": "Error downloading audio.",
	},
	"error_create_temp": {
		"ita": "Errore durante la creazione del file temporaneo.",
		"eng": "Error creating temporary file.",
	},
	"error_save_audio": {
		"ita": "Errore durante il salvataggio dell'audio.",
		"eng": "Error saving audio.",
	},
	"audio_playback_started": {
		"ita": "Done! I'm starting the audio playback!",
		"eng": "Done! I'm starting the audio playback!",
	},
	"audio_not_found": {
		"ita": "Audio non trovato.",
		"eng": "Audio not found.",
	},
	"playing_audio_button": {
		"ita": "Riproduco l'audio.",
		"eng": "Playing audio.",
	},
	"stopping_bot_button": {
		"ita": "Interrompo il bot.",
		"eng": "Stopping the bot.",
	},
	"queue_message": {
		"ita": "\n\nSe il server é sovraccarico, potrebbe volerci un po' di tempo\n*CPU: %s%% - RAM: %.2f%%*",
		"eng": "\n\nIf the server is overloaded, it might take a while\n*CPU: %s%% - RAM: %.2f%%*",
	},
	"discord_api_error": {
		"ita": "Discord API Error, per favore riprova piú tardi",
		"eng": "Discord API Error, please try again later",
	},
	"disagio": {
		"ita": "Disagio.",
		"eng": "Discomfort.",
	},
	"no_permissions_channel": {
		"ita": "Non hai i permessi per utilizzare questo comando in questo canale vocale.",
		"eng": "You don't have permissions to use this command in this voice channel.",
	},
	"initializing_connection": {
		"ita": "Per favore riprova piú tardi, Sto inizializzando la connessione...",
		"eng": "Please try again later, I'm initializing the connection...",
	},
	"bot_not_ready": {
		"ita": "Il bot non é ancora pronto opppure un altro user sta usando qualche altro comando.\nPer favore riprova piú tardi o utilizza il comando /stop",
		"eng": "The bot is not ready yet or another user is using some other command.\nPlease try again later or use the /stop command",
	},
	"no_sentence_found_with_text": {
		"ita": "Nessuna frase trovata contenente il testo \"%s\"",
		"eng": "No sentence found containing the text \"%s\"",
	},
	"error_fakeyou_requests": {
		"ita": "\n\nERROR: FakeYou sta ricevendo troppe richieste, prova a selezionare una voce diversa.",
		"eng": "\n\nERROR: FakeYou is receiving too many requests, try selecting a different voice.",
	},
	"error_audio_generation_voice": {
		"ita": "\n\nERROR: Errore nella generazione dell'audio, prova a selezionare una voce diversa.",
		"eng": "\n\nERROR: Error generating audio, try selecting a different voice.",
	},
	"error_audio_generation_retry": {
		"ita": "\n\nERROR: Errore nella generazione dell'audio, riprovare fra qualche istante.",
		"eng": "\n\nERROR: Error generating audio, please try again in a few moments.",
	},
	"error_text_too_long": {
		"ita": "Il testo è troppo lungo per la voce di Google (massimo 200 caratteri).",
		"eng": "The text is too long for Google voice (maximum 200 characters).",
	},
	"already_connected": {
		"ita": "Sono già connesso a questo canale",
		"eng": "I'm already connected to this channel",
	},
	"error_file_too_large": {
		"ita": "Il file è troppo grande.",
		"eng": "The file is too large.",
	},
}

// T translates a key to the language specified in the LANG environment variable.
// It falls back to "ita" if the language is not found or invalid.
func T(key string, args ...interface{}) string {
	lang := os.Getenv("LANG")
	if lang == "" {
		lang = "ita"
	}
	lang = strings.ToLower(lang)

	if langStrings, ok := translations[key]; ok {
		if val, ok := langStrings[lang]; ok {
			return fmt.Sprintf(val, args...)
		}
		if val, ok := langStrings["ita"]; ok {
			return fmt.Sprintf(val, args...)
		}
	}
	return key
}
