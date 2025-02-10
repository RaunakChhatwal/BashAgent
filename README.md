# BashAgent

BashAgent is the first AI agentic application to allow access to interactive bash commands and developer shells. It achieves this by having developed a feature into the Linux kernel to introspect the unix pipe between a bash process and its child processes. When a bash command expects input from the user, the kernel notifies the BashAgent server and the server in turn relays the prompt to the client.

## Demonstration
```
$ bash-agent --server 192.168.122.207 --model claude-3-5-sonnet-20241022
> please run ipython
I'll help you start an ipython session using the bash tool.

Running command `ipython.`
Python 3.12.8 (main, Dec  3 2024, 18:42:41) [GCC 14.2.1 20241116]
Type 'copyright', 'credits' or 'license' for more information
IPython 8.30.0 -- An enhanced Interactive Python. Type '?' for help.

In [1]:

.

An IPython session is now running. You can enter Python commands directly. What would you like to do in IPython?

> please get the millionth digit of pi using mpmath                  
I'll help you calculate this using mpmath in the IPython session. We'll need to use mp.pi and then convert it to a string to access specific digits.

Running command `from mpmath import mp
mp.dps = 1000000  # Set precision to at least 1 million digits
pi_str = str(mp.pi)
print(f"The millionth digit of pi is: {pi_str[1000000]}")  # Adding 1 to skip the "3.".`
In [2]: 
In [3]: 
In [4]: The millionth digit of pi is: 5

In [5]:

.

The millionth digit of pi is 5! 

If you'd like to verify this or explore other digits of pi, just let me know. We could also check digits around this position or calculate different positions in pi.

> please run sizeof(mp.pi)
I'll help you check the size of mp.pi using sys.getsizeof() since that's the proper way to check object size in Python.

Running command `import sys
print(sys.getsizeof(mp.pi)).`
In [6]: 56

In [7]:

.

The result shows 56 bytes, which might seem surprisingly small. This is because mpmath uses a special internal representation for its numbers, and mp.pi is likely a computed property rather than storing all digits in memory at once. The actual digits are computed as needed based on the precision settings.

To see more details about the mp.pi object, we could inspect it further. Would you like to see more information about how mpmath represents this value?
```

## Usage

Since this functionality depends on a custom Linux kernel feature, this application is split into client and server CLI programs, `bash-agent` and `bash-agent-server` respectively. `bash-agent` is cross-platform whereas `bash-agent-server` must run in a VM with the kernel patch. `bash-agent` takes as argument the VM's ip address and the model name (e.g. claude-3-5-sonnet-20241022).

## Installation

1) Install bash-agent either through the Nix flake or by cloning this repo and running `cargo build`
2) Download the disk image from the releases page and run `unxz nixos.qcow2.xz`
3) You can run nixos.qcow2 using virt-manager or use `qemu-img` to reformat it into VDI for VirtualBox.
4) Configure bridge networking for the VM
5) Run `python sandbox/setup.py <VM IP address>` after running the VM for the first time
6) SSH into the VM using username claude and password "mcdonalds"
7) Run `home-manager switch`
8) `cd` into ~/misc/system and run `sudo nix flake update bash-agent` followed by `sudo nixos-rebuild switch --flake .`
9) Run `bash-agent-server`, restart it whenever you want a new session