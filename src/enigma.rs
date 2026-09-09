//! Historically accurate Wehrmacht Enigma I: rotors I–V, reflectors B & C,
//! plugboard, ring settings, and the real double-stepping mechanism.
//!
//! The whole point of this module is `encode`, which does not just return the
//! output letter — it returns a `Trace` describing every contact-to-contact hop
//! the current takes through the machine. The TUI replays that trace to animate
//! the signal path. Keeping the math here (and pure) means it can be unit-tested
//! against known Enigma vectors independently of any rendering.

const A: u8 = b'A';

#[derive(Clone, Copy)]
pub struct RotorSpec {
    pub name: &'static str,
    pub wiring: &'static str, // forward wiring as seen entering from the right
    pub notch: u8,            // window position (0–25) that lets the next rotor step
}

// Standard rotor wirings. Notch letters: I→Q, II→E, III→V, IV→J, V→Z.
pub const ROTORS: [RotorSpec; 5] = [
    RotorSpec { name: "I",   wiring: "EKMFLGDQVZNTOWYHXUSPAIBRCJ", notch: 16 }, // Q
    RotorSpec { name: "II",  wiring: "AJDKSIRUXBLHWTMCQGZNPYFVOE", notch: 4  }, // E
    RotorSpec { name: "III", wiring: "BDFHJLCPRTXVZNYEIWGAKMUSQO", notch: 21 }, // V
    RotorSpec { name: "IV",  wiring: "ESOVPZJAYQUIRHXLNFTGKDCMWB", notch: 9  }, // J
    RotorSpec { name: "V",   wiring: "VZBRGITYUPSDNHLXAWMJQOFECK", notch: 25 }, // Z
];

pub const REFLECTOR_B: &str = "YRUHQSLDPXNGOKMIEBFZCWVJAT";
pub const REFLECTOR_C: &str = "FVPJIAOYEDRZXWGCTKUQSBNMHL";

/// Selectable reflectors, by name. Enigma I shipped with UKW-B and UKW-C.
pub const REFLECTORS: [(&str, &str); 2] = [("B", REFLECTOR_B), ("C", REFLECTOR_C)];

#[derive(Clone)]
pub struct Rotor {
    pub spec: RotorSpec,
    forward: [u8; 26], // contact index -> contact index, right to left
    inverse: [u8; 26], // the reverse permutation, for the return trip
    pub position: u8,  // 0–25, the letter currently in the window
    pub ring: u8,      // 0–25, Ringstellung
}

impl Rotor {
    pub fn new(spec: RotorSpec, position: u8, ring: u8) -> Self {
        let mut forward = [0u8; 26];
        let mut inverse = [0u8; 26];
        for (i, c) in spec.wiring.bytes().enumerate() {
            let o = c - A;
            forward[i] = o;
            inverse[o as usize] = i as u8;
        }
        Rotor { spec, forward, inverse, position, ring }
    }

    /// Net rotation of the wiring relative to the entry contacts.
    fn offset(&self) -> i32 {
        (self.position as i32 - self.ring as i32).rem_euclid(26)
    }

    pub fn fwd(&self, c: u8) -> u8 {
        let off = self.offset();
        let entry = ((c as i32 + off).rem_euclid(26)) as usize;
        ((self.forward[entry] as i32 - off).rem_euclid(26)) as u8
    }

    pub fn bwd(&self, c: u8) -> u8 {
        let off = self.offset();
        let entry = ((c as i32 + off).rem_euclid(26)) as usize;
        ((self.inverse[entry] as i32 - off).rem_euclid(26)) as u8
    }

    pub fn at_notch(&self) -> bool {
        self.position == self.spec.notch
    }

    pub fn step(&mut self) {
        self.position = (self.position + 1) % 26;
    }

    pub fn window(&self) -> char {
        (A + self.position) as char
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Stage {
    PlugIn,
    Rotor(usize),     // rotors[i] on the right-to-left leg
    Reflector,
    RotorBack(usize), // rotors[i] on the left-to-right leg
    PlugOut,
    Lamp,
}

#[derive(Clone, Copy)]
pub struct Hop {
    pub stage: Stage,
    pub input: u8,
    pub output: u8,
}

pub struct Trace {
    pub key: u8,
    pub stepped: [bool; 3], // which of [left, middle, right] advanced this press
    pub hops: Vec<Hop>,
    pub lamp: u8,
}

pub struct Machine {
    pub rotors: [Rotor; 3], // [left (slow), middle, right (fast)]
    reflector: [u8; 26],
    plugboard: [u8; 26],
}

impl Machine {
    pub fn new(rotors: [Rotor; 3], reflector: &str) -> Self {
        let mut r = [0u8; 26];
        for (i, c) in reflector.bytes().enumerate() {
            r[i] = c - A;
        }
        let mut plugboard = [0u8; 26];
        for i in 0..26 {
            plugboard[i] = i as u8; // identity until pairs are set
        }
        Machine { rotors, reflector: r, plugboard }
    }

    /// Wire up plugboard pairs, e.g. &[("A","B"), ("C","D")].
    pub fn set_plugs(&mut self, pairs: &[(u8, u8)]) {
        for &(a, b) in pairs {
            self.plugboard[a as usize] = b;
            self.plugboard[b as usize] = a;
        }
    }

    /// The real stepping logic, including the double-step anomaly:
    /// the middle rotor advances when the right rotor is at its notch OR when
    /// the middle rotor is itself at its notch — and in that second case it
    /// drags the left rotor along too. Notches are read BEFORE anything moves.
    fn step_rotors(&mut self) -> [bool; 3] {
        let right_notch = self.rotors[2].at_notch();
        let middle_notch = self.rotors[1].at_notch();

        let step_left = middle_notch;
        let step_middle = right_notch || middle_notch;

        if step_left  { self.rotors[0].step(); }
        if step_middle { self.rotors[1].step(); }
        self.rotors[2].step();

        [step_left, step_middle, true]
    }

    /// Encrypt one letter (0–25) and record the full signal path.
    pub fn encode(&mut self, key: u8) -> Trace {
        let stepped = self.step_rotors();
        let mut hops = Vec::with_capacity(9);
        let mut c = key;

        let p = self.plugboard[c as usize];
        hops.push(Hop { stage: Stage::PlugIn, input: c, output: p });
        c = p;

        for &i in &[2usize, 1, 0] {
            let o = self.rotors[i].fwd(c);
            hops.push(Hop { stage: Stage::Rotor(i), input: c, output: o });
            c = o;
        }

        let r = self.reflector[c as usize];
        hops.push(Hop { stage: Stage::Reflector, input: c, output: r });
        c = r;

        for &i in &[0usize, 1, 2] {
            let o = self.rotors[i].bwd(c);
            hops.push(Hop { stage: Stage::RotorBack(i), input: c, output: o });
            c = o;
        }

        let p2 = self.plugboard[c as usize];
        hops.push(Hop { stage: Stage::PlugOut, input: c, output: p2 });
        c = p2;

        hops.push(Hop { stage: Stage::Lamp, input: c, output: c });
        Trace { key, stepped, hops, lamp: c }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_machine() -> Machine {
        // Rotors I II III (left→right), reflector B, rings AAA, positions AAA.
        let rotors = [
            Rotor::new(ROTORS[0], 0, 0),
            Rotor::new(ROTORS[1], 0, 0),
            Rotor::new(ROTORS[2], 0, 0),
        ];
        Machine::new(rotors, REFLECTOR_B)
    }

    #[test]
    fn canonical_vector() {
        // The textbook I–II–III / B / AAA / AAA vector: 25 A's should give
        // BDZGOWCXLTKSBTMCDLPBMUQOF. This exercises the double-step too.
        let mut m = default_machine();
        let out: String = (0..25)
            .map(|_| (b'A' + m.encode(0).lamp) as char)
            .collect();
        assert_eq!(out, "BDZGOWCXLTKSBTMCDLPBMUQOF");
    }

    #[test]
    fn reciprocal() {
        // Enigma is its own inverse with the same start settings.
        let plaintext = "ENIGMAREVEALED";
        let mut enc = default_machine();
        let cipher: Vec<u8> = plaintext.bytes().map(|b| enc.encode(b - b'A').lamp).collect();

        let mut dec = default_machine();
        let back: String = cipher.iter().map(|&c| (b'A' + dec.encode(c).lamp) as char).collect();
        assert_eq!(back, plaintext);
    }
}
