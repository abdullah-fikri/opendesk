package logger

import (
	"fmt"
	"os"
	"sync"
	"time"
)

var (
	mu  sync.Mutex
	pid = os.Getpid()
)

func write(level, subsystem, msg string) {
	now := time.Now().Format("2006-01-02 15:04:05.000")
	mu.Lock()
	fmt.Fprintf(os.Stdout, "%7s %s  relay %6d  %30s - %s\n", level, now, pid, subsystem, msg)
	mu.Unlock()
}

func Info(subsystem, format string, args ...interface{}) {
	write("info", subsystem, fmt.Sprintf(format, args...))
}

func Warn(subsystem, format string, args ...interface{}) {
	write("warning", subsystem, fmt.Sprintf(format, args...))
}

func Error(subsystem, format string, args ...interface{}) {
	write("error", subsystem, fmt.Sprintf(format, args...))
}
