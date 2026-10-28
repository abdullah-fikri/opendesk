// Package version exposes the build version of the relay.
package version

// Version is the semantic version of this build. Release builds override it with
// -ldflags "-X opendesk/server-relay/internal/version.Version=<tag>".
var Version = "1.1.2"
