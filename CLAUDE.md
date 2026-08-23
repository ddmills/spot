# Project Guidelines

- **Style**: Prioritize self-documenting code with clear variable and function names over explanatory comments.
- **Output**: Provide only the requested code changes without narrative explanations or summaries. Respond only in ASD-STE100 or Simplified Technical English.

**Comment Guidelines**
- Use comments sparingly
- Use acive voice in comments
- Only document non-obvious architectural decisions or complex constraints.
- Don't comment out code. Remove it instead
- Don't add comments that describe the process of changing code
- Don't add comments that emphasize different versions of the code, like "this code now handles"
- Don't use end-of-line comments. Place comments above the code they describe
- Don't include past tense verbs like added, removed, or changed

Example: `this.timeout(10_000); // Increase timeout for API calls`

This is bad because a reader doesn't know what the timeout was increased from, and doesn't care about the old behavior
