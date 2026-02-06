use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::Dictionary;
use tempfile::TempDir;
use std::collections::HashSet;

fn generate_diverse_terms(count: usize) -> Vec<String> {
    let mut terms = Vec::with_capacity(count);
    let alphabet: Vec<char> = ('a'..='z').collect();
    let mut i = 0;
    for suffix in 0..256 {
        for c1 in &alphabet {
            for c2 in &alphabet {
                if i >= count {
                    return terms;
                }
                terms.push(format!("{}{}{:04}", c1, c2, suffix));
                i += 1;
            }
        }
    }
    terms
}

fn main() {
    let temp_dir = TempDir::new().expect("temp dir");
    let path = temp_dir.path().join("debug_test");
    
    // Use fewer terms to debug
    let terms = generate_diverse_terms(1000);
    
    // Phase 1: Insert all terms and verify in memory
    {
        let mut dict = PersistentARTrie::<()>::create(&path).expect("create dict");
        
        for term in &terms {
            dict.insert(term);
        }
        
        let unique_count = terms.iter().collect::<HashSet<_>>().len();
        println!("Inserted {} terms", unique_count);
        assert_eq!(dict.len(), Some(unique_count), "Length should match");
        
        // Count by first letter
        let mut by_letter: std::collections::HashMap<char, usize> = std::collections::HashMap::new();
        for t in &terms {
            *by_letter.entry(t.chars().next().unwrap()).or_insert(0) += 1;
        }
        
        // Count which letters exist in dict
        println!("BEFORE CHECKPOINT - verifying first letters:");
        for c in 'a'..='z' {
            let test_term = format!("{}a0000", c);
            let exists = dict.contains(&test_term);
            let count = by_letter.get(&c).copied().unwrap_or(0);
            if exists {
                print!("{}:{}/OK ", c, count);
            } else if count > 0 {
                print!("{}:{}/MISS ", c, count);
            }
        }
        println!();
        
        dict.checkpoint().expect("checkpoint");
        dict.sync().expect("sync");
    }
    
    // Phase 2: Reopen and verify
    {
        let dict = PersistentARTrie::<()>::open(&path).expect("reopen");
        
        println!("\nAFTER REOPEN: len = {:?}", dict.len());
        
        println!("After reopen - verifying first letters:");
        for c in 'a'..='z' {
            let test_term = format!("{}a0000", c);
            if dict.contains(&test_term) {
                print!("{}:OK ", c);
            } else {
                print!("{}:MISS ", c);
            }
        }
        println!();
        
        // Full verification
        let mut missing = Vec::new();
        for term in &terms {
            if !dict.contains(term) {
                missing.push(term.clone());
            }
        }
        
        if !missing.is_empty() {
            println!("\nMISSING TERMS ({} total, first 20):", missing.len());
            for t in missing.iter().take(20) {
                println!("  {}", t);
            }
        } else {
            println!("\nSUCCESS: All terms verified!");
        }
    }
}
