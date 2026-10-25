// Package model holds the relay's domain entities and rules. It has no knowledge of
// transport (WebSocket, JSON) or storage (PostgreSQL).
package model

// DeskIDLength is the number of digits in a canonical desk ID.
const DeskIDLength = 9

// CleanDeskID strips every non-digit from raw, turning "123 456 789" into "123456789".
func CleanDeskID(raw string) string {
	out := make([]byte, 0, len(raw))
	for i := 0; i < len(raw); i++ {
		if c := raw[i]; c >= '0' && c <= '9' {
			out = append(out, c)
		}
	}
	return string(out)
}

// FormatDeskID renders a desk ID in its display form "123 456 789". Shorter input is returned as bare digits.
func FormatDeskID(raw string) string {
	digits := CleanDeskID(raw)
	if len(digits) < DeskIDLength {
		return digits
	}
	return digits[0:3] + " " + digits[3:6] + " " + digits[6:9]
}

// IsValidDeskID reports whether clean is a registrable desk ID: exactly nine digits without a leading zero.
func IsValidDeskID(clean string) bool {
	return len(clean) == DeskIDLength && clean[0] != '0' && CleanDeskID(clean) == clean
}
