package main

import (
	"bytes"
	"crypto/md5"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"math/rand"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"time"

	"github.com/google/uuid"
)

var fakeYouVoices = map[string]string{
	"Papa Francesco (FakeYou.com)":   "weight_gc8gsr41974q5ax35gvttr85v",
	"Silvio Berlusconi (FakeYou.com)": "weight_324nvat7xvaawe146na154gwh",
	"Goku (FakeYou.com)":              "weight_wn689844yyr08jny6jyyvkwcp",
	"Gerry Scotti (FakeYou.com)":      "weight_ms1kzt5m09cfw1yn666cxhy88",
	"Peter Griffin (FakeYou.com)":    "weight_t0y9rpba3qjnq02da44ynfs45",
	"Homer Simpson (FakeYou.com)":    "weight_zw97bw3hbtm07qwkd2exna15b",
}

var httpClient = &http.Client{
	Timeout: 60 * time.Second,
}

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

type fakeYouInferenceRequest struct {
	TtsModelToken        string `json:"tts_model_token"`
	UuidIdempotencyToken string `json:"uuid_idempotency_token"`
	InferenceText        string `json:"inference_text"`
}

type fakeYouInferenceResponse struct {
	Success  bool   `json:"success"`
	JobToken string `json:"job_token"`
}

type fakeYouTaskResponse struct {
	Success bool `json:"success"`
	Status  struct {
		Status string `json:"status"`
	} `json:"status"`
	PredictionToken string `json:"prediction_token"`
}

// GetTTSFakeYou fetches TTS audio from the FakeYou API.
func GetTTSFakeYou(text string, voice string) ([]byte, error) {
	voiceToken, ok := fakeYouVoices[voice]
	if !ok {
		return nil, fmt.Errorf("invalid fakeyou voice: %s", voice)
	}

	// 1. Create inference
	reqBody := fakeYouInferenceRequest{
		TtsModelToken:        voiceToken,
		UuidIdempotencyToken: uuid.NewString(),
		InferenceText:        text,
	}
	bodyBytes, _ := json.Marshal(reqBody)

	req, err := http.NewRequest("POST", "https://api.fakeyou.com/tts/", bytes.NewBuffer(bodyBytes))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")

	resp, err := httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fakeyou inference failed with status: %s", resp.Status)
	}

	var infResp fakeYouInferenceResponse
	if err := json.NewDecoder(resp.Body).Decode(&infResp); err != nil {
		return nil, err
	}
	if !infResp.Success || infResp.JobToken == "" {
		return nil, fmt.Errorf("fakeyou inference failed to return job token")
	}
	LogDebug("FakeYou inference created for voice %s, job token: %s", voice, infResp.JobToken)

	// 2. Poll for result
	var predictionToken string
	for i := 0; i < 60; i++ { // max 60 attempts (2 minutes)
		time.Sleep(2 * time.Second)
		
		taskReq, _ := http.NewRequest("GET", "https://api.fakeyou.com/task/"+infResp.JobToken, nil)
		taskReq.Header.Set("Accept", "application/json")
		taskReq.Header.Set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
		
		taskResp, err := httpClient.Do(taskReq)
		if err != nil {
			continue
		}
		
		var taskData fakeYouTaskResponse
		json.NewDecoder(taskResp.Body).Decode(&taskData)
		taskResp.Body.Close()

		if taskData.Status.Status == "result_success" {
			predictionToken = taskData.PredictionToken
			LogDebug("FakeYou task succeeded, prediction token: %s", predictionToken)
			break
		} else if taskData.Status.Status == "result_failure" {
			LogError("FakeYou task failed")
			return nil, fmt.Errorf("fakeyou task failed")
		}
	}

	if predictionToken == "" {
		LogError("FakeYou task timed out")
		return nil, fmt.Errorf("fakeyou task timed out")
	}

	// 3. Get audio
	audioReq, _ := http.NewRequest("GET", "https://api.fakeyou.com/tts/result/"+predictionToken, nil)
	audioReq.Header.Set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
	
	audioResp, err := httpClient.Do(audioReq)
	if err != nil {
		return nil, err
	}
	defer audioResp.Body.Close()

	if audioResp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fakeyou audio fetch failed with status: %s", audioResp.Status)
	}

	return io.ReadAll(audioResp.Body)
}

// SaveAudio writes the audio bytes to the specified file path.
func SaveAudio(filePath string, data []byte) error {
	if err := os.MkdirAll(filepath.Dir(filePath), 0755); err != nil {
		return err
	}
	return os.WriteFile(filePath, data, 0644)
}

// GetRandomVoice returns a random voice from the available voices including Google.
func GetRandomVoice() string {
	voices := []string{"Google"}
	for voice := range fakeYouVoices {
		voices = append(voices, voice)
	}
	return voices[rand.Intn(len(voices))]
}
