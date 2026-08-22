package main

import (
	"log"
	"os"
	"strings"
)

var currentLogLevel = "info"

// InitLogger initializes the log level from the LOG_LEVEL environment variable.
func InitLogger() {
	level := os.Getenv("LOG_LEVEL")
	if level == "" {
		level = "info"
	}
	currentLogLevel = strings.ToLower(level)
}

// LogDebug logs a message if the log level is debug.
func LogDebug(format string, v ...interface{}) {
	if currentLogLevel == "debug" {
		log.Printf("[DEBUG] "+format, v...)
	}
}

// LogInfo logs a message if the log level is debug or info.
func LogInfo(format string, v ...interface{}) {
	if currentLogLevel == "debug" || currentLogLevel == "info" {
		log.Printf("[INFO] "+format, v...)
	}
}

// LogError logs a message if the log level is debug, info, or error.
func LogError(format string, v ...interface{}) {
	if currentLogLevel == "debug" || currentLogLevel == "info" || currentLogLevel == "error" {
		log.Printf("[ERROR] "+format, v...)
	}
}
