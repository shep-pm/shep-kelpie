// Package ecosystem is a fixture: one dependency, one test.
package ecosystem

import "github.com/google/uuid"

// ID is the same UUID for the same name.
func ID(name string) string {
	return uuid.NewSHA1(uuid.NameSpaceDNS, []byte(name)).String()
}
