package main

import (
	"crypto/md5"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
)

// ComputeMD5Hash generates an MD5 hash for a given string to use as a filename.
func ComputeMD5Hash(text string) string {
	hasher := md5.New()
	hasher.Write([]byte(text))
	return hex.EncodeToString(hasher.Sum(nil))
}

// GetAudioFilePath returns the standardized path for a saved audio file.
func GetAudioFilePath(text string, voice string) string {
	return filepath.Join("audios", voice+"_"+ComputeMD5Hash(text)+".mp3")
}

// GetTTSGoogle fetches TTS audio from the unofficial Google Translate endpoint.
func GetTTSGoogle(text string) ([]byte, error) {
	resp, err := http.Get("https://translate.google.com/translate_tts?ie=UTF-8&tl=it&client=tw-ob&q=" + url.QueryEscape(text))
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("google tts failed with status: %s", resp.Status)
	}
	return io.ReadAll(resp.Body)
}

// GetTTSFakeYou will be implemented in the next step using the FakeYou API.
func GetTTSFakeYou(text string, voice string) ([]byte, error) {
	return nil, fmt.Errorf("FakeYou not implemented yet")
}

// SaveAudio writes the audio bytes to the specified file path.
func SaveAudio(filePath string, data []byte) error {
	if err := os.MkdirAll(filepath.Dir(filePath), 0755); err != nil {
		return err
	}
	return os.WriteFile(filePath, data, 0644)
}
