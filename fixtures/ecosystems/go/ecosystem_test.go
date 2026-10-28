package ecosystem

import "testing"

func TestIDIsAFunctionOfTheName(t *testing.T) {
	first, again, other := ID("kelpie.test"), ID("kelpie.test"), ID("other.test")
	if first != again || first == other {
		t.Fatalf("ID is not a function of its name: %s %s %s", first, again, other)
	}
}
