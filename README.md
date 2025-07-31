# Sliding Window Leader Election (SWLE)



This repository contains supplementary artifacts (code + paper) of SWLE, a novel reputation-based leader election mechanism for partially synchronous Byzantine Fault Tolerant (BFT) protocols.



## Paper



This supplementary artifacts accompanies the paper:

**"Reputation-Based Leader Election under Partial Synchrony: Towards a Protocol-Independent Abstraction with Enhanced Guarantees"**

submitted to INFOCOM 2026.


## Repository Structure


```
├── src/
│   ├── swle.rs          # Core SWLE implementation (relies on crypto.rs alongside standard library dependencies, and is protocol-independent)
│   ├── crypto.rs        # Cryptographic primitives
│   └── metrics.rs       # Performance measurement module (depends on block metadata and configuration parameters (e.g., sliding window size), so it is not protocol-independent)
├── paper/
│   └── swle.pdf         # The paper (with full proofs)
└── README.md
```


## Files Description


### Core Implementation

- **`swle.rs`**: Contains the complete SWLE mechanism including reputation scoring, candidate selection, and leader certificate generation. Implements all algorithms described in the paper.



- **`crypto.rs`**: Provides cryptographic primitives built primarily using `ed25519_dalek`, including key generation, digital signatures, and hash functions.



- **`metrics.rs`**: Performance measurement module for throughput and latency analysis. Includes both global average and instantaneous metrics with sliding window sampling.





## Usage



This implementation serves as a research prototype. To integrate SWLE into a leader-based BFT protocol:



1. Include the `swle.rs` module in your project

2. Initialize SWLE with your voter set and protocol parameters

3. Call appropriate reputation update methods during consensus

4. Use SWLE's leader determination for view progression



---



**Note**: This repository is maintained for research reproducibility. For the latest updates and detailed analysis, please refer to the accompanying paper.





