package config

import (
	"encoding/json"
	"os"
	"strconv"
)

type Config struct {
	Host                string `json:"host"`
	Port                int    `json:"port"`
	PingIntervalSeconds int    `json:"ping_interval_seconds"`
	ReadBufferSize      int    `json:"read_buffer_size"`
	WriteBufferSize     int    `json:"write_buffer_size"`
	DBHost              string `json:"db_host"`
	DBPort              int    `json:"db_port"`
	DBUser              string `json:"db_user"`
	DBPassword          string `json:"db_password"`
	DBName              string `json:"db_name"`
}

func Load(path string) Config {
	cfg := Config{
		Host:                "0.0.0.0",
		Port:                8088,
		PingIntervalSeconds: 15,
		ReadBufferSize:      32768,
		WriteBufferSize:     32768,
		DBHost:              "localhost",
		DBPort:              5432,
		DBUser:              "postgres",
		DBPassword:          "postgres",
		DBName:              "opendesk",
	}

	if data, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(data, &cfg)
	}

	if envHost := os.Getenv("DB_HOST"); envHost != "" {
		cfg.DBHost = envHost
	}
	if envPort := os.Getenv("DB_PORT"); envPort != "" {
		if p, err := strconv.Atoi(envPort); err == nil {
			cfg.DBPort = p
		}
	}
	if envUser := os.Getenv("DB_USER"); envUser != "" {
		cfg.DBUser = envUser
	}
	if envPass := os.Getenv("DB_PASSWORD"); envPass != "" {
		cfg.DBPassword = envPass
	}
	if envName := os.Getenv("DB_NAME"); envName != "" {
		cfg.DBName = envName
	}

	return cfg
}
