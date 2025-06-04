# Design

Historical lessons and findings along with remaining open questions.

## Connection

At some point it will probably make sense to have an event loop in the `Connection` struct that handles certain interactions automatically, like the ping/pongs. `Connection` is currently still used as a short-lived interaction, but if this changes, that is probably when it should be improved. Perhaps both scenarios can be well supported by adding some sort of "listen" method which fires up an event loop and tosses back requested message types over a channel.
