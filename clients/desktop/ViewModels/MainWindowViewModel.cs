using System;
using System.Collections.ObjectModel;
using System.Linq;
using System.Net;
using System.Threading;
using System.Threading.Tasks;
using System.Collections.Generic;
using Aura.Desktop.Services;
using Aura.V1Alpha1;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace Aura.Desktop.ViewModels;

/// <summary>
/// Main ViewModel for the Aura desktop client.
/// Handles connection, authentication, and voice streaming.
/// </summary>
public partial class MainWindowViewModel : ObservableObject, IAsyncDisposable
{
    private AuraNetworkClient? _client;
    private UserIdentity? _identity;
    private RustAudioEngine? _audioEngine;
    private AudioManager? _audioManager;
    private CancellationTokenSource? _audioCts;
    
    // ==========================================================================
    // Observable Properties
    // ==========================================================================
    
    [ObservableProperty]
    private bool _isConnected;
    
    [ObservableProperty]
    private bool _isAuthenticated;
    
    [ObservableProperty]
    private string _connectionStatus = "Disconnected";
    
    [ObservableProperty]
    private string _serverAddress = "127.0.0.1";
    
    [ObservableProperty]
    private int _serverPort = 8443;
    
    [ObservableProperty]
    private string _serverPassword = "";
    
    [ObservableProperty]
    private string _displayName = "";
    
    [ObservableProperty]
    private string _publicKeyDisplay = "";
    
    [ObservableProperty]
    private bool _isMicEnabled;
    
    [ObservableProperty]
    private bool _isDeafened;
    
    [ObservableProperty]
    private string _audioStats = "";
    
    [ObservableProperty]
    private Channel? _selectedChannel;
    
    [ObservableProperty]
    private ObservableCollection<ChatMessage> _messages = new();
    
    [ObservableProperty]
    private ObservableCollection<Channel> _channels = new();
    
    [ObservableProperty]
    private string _messageInput = "";
    
    // ==========================================================================
    // Audio Settings
    // ==========================================================================
    
    [ObservableProperty]
    private bool _rnnoiseEnabled = true;
    
    [ObservableProperty]
    private bool _aecEnabled = true;
    
    [ObservableProperty]
    private bool _webrtcNsEnabled = false;
    
    [ObservableProperty]
    private bool _agcEnabled = true;
    
    [ObservableProperty]
    private int _dredDuration = 10;  // 100ms
    
    [ObservableProperty]
    private int _jitterBufferMs = 40;
    
    [ObservableProperty]
    private bool _showAudioSettings = false;
    
    partial void OnRnnoiseEnabledChanged(bool value) => _audioManager?.SetNoiseSuppressionEnabled(value);
    partial void OnAecEnabledChanged(bool value) => _audioManager?.SetWebrtcAecEnabled(value);
    partial void OnWebrtcNsEnabledChanged(bool value)
    {
        _audioManager?.SetWebrtcNsEnabled(value);
        // Auto-disable RNNoise when WebRTC NS is enabled
        if (value) RnnoiseEnabled = false;
    }
    partial void OnAgcEnabledChanged(bool value) => _audioManager?.SetWebrtcAgcEnabled(value);
    partial void OnDredDurationChanged(int value) => _audioManager?.SetDredDuration(value);
    partial void OnJitterBufferMsChanged(int value) => _audioManager?.SetJitterBufferMs((uint)value);
    
    // ==========================================================================
    // Initialization
    // ==========================================================================
    
    public MainWindowViewModel()
    {
        // Try to load libmsquic explicitly on macOS
        MsQuicLoader.TryLoadMsQuic();
        
        // Check QUIC support
        QuicSupport.CheckQuicSupport();
        
        // Try to load existing identity
        var identityPath = UserIdentity.GetIdentityFilePath();
        try
        {
            if (System.IO.File.Exists(identityPath))
            {
                _identity = UserIdentity.Load(identityPath);
                DisplayName = _identity.DisplayName;
                PublicKeyDisplay = _identity.PublicKeyHex[..16] + "...";
                ConnectionStatus = "Ready (identity loaded)";
            }
            else
            {
                ConnectionStatus = "Ready (no identity - will generate on connect)";
            }
        }
        catch (Exception ex)
        {
            ConnectionStatus = $"Error loading identity: {ex.Message}";
        }
        
        // Initialize with default channel
        Channels = new ObservableCollection<Channel>
        {
            new Channel 
            { 
                Id = "1", 
                Name = "General", 
                IsExpanded = true,
                Users = new ObservableCollection<User>()
            }
        };
    }
    
    // ==========================================================================
    // Commands
    // ==========================================================================
    
    [RelayCommand]
    private async Task ConnectAsync()
    {
        if (string.IsNullOrWhiteSpace(DisplayName))
        {
            ConnectionStatus = "Error: Display name required";
            return;
        }
        
        try
        {
            Console.WriteLine("[ViewModel] Starting connection...");
            
            // 1. Generate or load identity
            _identity ??= UserIdentity.LoadOrCreate(DisplayName);
            _identity.DisplayName = DisplayName;
            PublicKeyDisplay = _identity.PublicKeyHex[..16] + "...";
            Console.WriteLine($"[ViewModel] Identity loaded: {_identity.PublicKeyHex[..16]}...");
            
            // 2. Create client and connect
            _client = new AuraNetworkClient();
            _audioEngine ??= new RustAudioEngine();
            Console.WriteLine("[ViewModel] Creating AudioManager...");
            NativeLibraryLoader.Initialize();
            _audioManager ??= new AudioManager();
            Console.WriteLine("[ViewModel] AudioManager created");
            _client.SetAudioEngine(_audioEngine);
            _client.SetAudioManager(_audioManager);
            Console.WriteLine("[ViewModel] Audio components wired");
            
            // Listen for active speaker changes
            _audioManager.OnActiveSpeakersChanged += speakers =>
                Dispatcher.UIThread.Post(() => UpdateSpeakingIndicators(speakers));
            _client.OnStatusChanged += status => 
                Dispatcher.UIThread.Post(() => ConnectionStatus = status);
            _client.OnError += error => 
                Dispatcher.UIThread.Post(() => ConnectionStatus = $"Error: {error}");
            
            _client.OnUserJoined += (cid, sid, name) => 
                Dispatcher.UIThread.Post(() => HandleUserJoined(cid, sid, name));
            _client.OnUserLeft += (cid, sid) => 
                Dispatcher.UIThread.Post(() => HandleUserLeft(cid, sid));
            _client.OnServerSnapshot += snapshot => 
                Dispatcher.UIThread.Post(() => HandleServerSnapshot(snapshot));
            
            _client.OnTextMessage += (mid, sid, cid, content, reply) => 
                Dispatcher.UIThread.Post(() => HandleTextMessage(mid, sid, cid, content, reply));
            
            _client.OnUserStatusUpdated += (sid, muted, deafened) =>
                Dispatcher.UIThread.Post(() => HandleUserStatusUpdate(sid, muted, deafened));
            
            ConnectionStatus = "Connecting...";
            Console.WriteLine($"[ViewModel] Connecting to {ServerAddress}:{ServerPort}...");
            await _client.ConnectAsync(ServerAddress, ServerPort);
            IsConnected = true;
            Console.WriteLine($"[ViewModel] IsConnected = {IsConnected}");
            
            // 3. Authenticate with TOFU
            ConnectionStatus = "Authenticating...";
            Console.WriteLine("[ViewModel] Starting authentication...");
            var password = string.IsNullOrWhiteSpace(ServerPassword) ? null : ServerPassword;
            await _client.AuthenticateAsync(_identity, password);
            Console.WriteLine("[ViewModel] Authentication completed!");
            IsAuthenticated = true;
            Console.WriteLine($"[ViewModel] IsAuthenticated = {IsAuthenticated}, UserId = {_client.UserId}");
            
            ConnectionStatus = $"Connected as {DisplayName} (ID: {_client.UserId})";
            Console.WriteLine($"[ViewModel] ConnectionStatus = {ConnectionStatus}");
            
            Messages.Add(new ChatMessage 
            { 
                Content = $"Connected to {ServerAddress}:{ServerPort}",
                System = true 
            });
            
            Console.WriteLine("[ViewModel] Connection complete!");
        }
        catch (AuthenticationException ex)
        {
            Console.WriteLine($"[ViewModel] AUTH EXCEPTION: {ex.Message}");
            Console.WriteLine($"[ViewModel] Stack trace: {ex.StackTrace}");
            ConnectionStatus = $"Auth failed: {ex.Message}";
            await DisconnectInternalAsync();
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[ViewModel] CONNECTION EXCEPTION: {ex.GetType().Name}: {ex.Message}");
            Console.WriteLine($"[ViewModel] Stack trace: {ex.StackTrace}");
            if (ex.InnerException != null)
            {
                Console.WriteLine($"[ViewModel] INNER EXCEPTION: {ex.InnerException.GetType().Name}: {ex.InnerException.Message}");
                Console.WriteLine($"[ViewModel] Inner stack trace: {ex.InnerException.StackTrace}");
            }
            ConnectionStatus = $"Connection failed: {ex.Message}";
            await DisconnectInternalAsync();
        }
    }

    [RelayCommand]
    private async Task DisconnectAsync()
    {
        await DisconnectInternalAsync();
        ConnectionStatus = "Disconnected";
        Messages.Add(new ChatMessage 
        { 
            Content = "Disconnected from server",
            System = true 
        });
    }
    
    private async Task DisconnectInternalAsync()
    {
        StopMic();
        
        if (_client != null)
        {
            await _client.DisposeAsync();
            _client = null;
        }
        
        IsConnected = false;
        IsAuthenticated = false;
        Channels.Clear();
    }
    
    [RelayCommand]
    private async Task JoinChannelAsync(Channel? channel)
    {
        if (channel == null || _client == null) return;
        
        try
        {
            SelectedChannel = channel;
            channel.IsExpanded = true;
            
            await _client.JoinChannelAsync(uint.Parse(channel.Id));
            
            Messages.Add(new ChatMessage 
            { 
                Content = $"Joined channel: {channel.Name}",
                System = true 
            });
        }
        catch (Exception ex)
        {
            ConnectionStatus = $"Failed to join channel: {ex.Message}";
        }
    }
    
    [RelayCommand]
    private async Task ToggleMicrophoneAsync()
    {
        if (IsDeafened && !IsMicEnabled) return;
        
        IsMicEnabled = !IsMicEnabled;
        if (IsMicEnabled)
        {
            StartMic();
        }
        else
        {
            StopMic();
        }
        
        if (_client != null)
        {
            await _client.UpdateStatusAsync(!IsMicEnabled, IsDeafened);
        }
    }
    
    [RelayCommand]
    private async Task ToggleDeafenAsync()
    {
        IsDeafened = !IsDeafened;
        
        if (IsDeafened && IsMicEnabled)
        {
            // Auto-mute on deafen
            await ToggleMicrophoneAsync();
        }
        else if (_client != null)
        {
            await _client.UpdateStatusAsync(!IsMicEnabled, IsDeafened);
        }
        
        // TODO: Actually mute output in RustAudioEngine if needed
    }
    
    private void StartMic()
    {
        if (_audioEngine == null || _client == null) return;
        
        _audioCts = new CancellationTokenSource();
        
        _audioEngine.OnAudioData += d =>
        {
            if (_client != null && IsAuthenticated)
            {
                // Off-thread send to avoid blocking engine
                _ = _client.SendAudioFrameAsync(d, _audioCts.Token);
            }
        };
        
        _audioEngine.OnError += error =>
            Dispatcher.UIThread.Post(() => ConnectionStatus = $"Audio error: {error}");
        
        _audioEngine.StartCapture();
        
        Messages.Add(new ChatMessage 
        { 
            Content = "Rust Audio Engine enabled (CPAL)",
            System = true 
        });
    }
    
    private void StopMic()
    {
        _audioCts?.Cancel();
        _audioEngine?.StopCapture();
        _audioCts = null;
        AudioStats = "";
    }
    
    [RelayCommand]
    private async Task SendMessage()
    {
        if (string.IsNullOrWhiteSpace(MessageInput)) return;
        if (_client == null || SelectedChannel == null) return;

        var content = MessageInput;
        MessageInput = ""; // Clear input immediately

        try 
        {
            uint channelId = uint.Parse(SelectedChannel.Id);
            string msgId = Guid.NewGuid().ToString();

            // Optimistic Add
            Messages.Add(new ChatMessage 
            { 
                UserId = _client.UserId,
                UserName = "You",
                Content = content,
                IsFromCurrentUser = true
            });

            await _client.SendTextMessageAsync(channelId, content, msgId);
        }
        catch (Exception ex)
        {
            Messages.Add(new ChatMessage { Content = $"Failed to send: {ex.Message}", System = true });
        }
    }

    private void HandleTextMessage(string msgId, uint senderId, uint channelId, string content, string? replyToId)
    {
        // Don't show own messages again
        if (_client != null && senderId == _client.UserId) return;

        // Only show if for current channel or specific logic?
        // For simple parity, just show it.
        
        string senderName = $"User {senderId}";
        
        // Try to find name in channel
        var channel = Channels.FirstOrDefault(c => c.Id == channelId.ToString());
        if (channel != null)
        {
            var user = channel.Users.FirstOrDefault(u => u.Id == senderId);
            if (user != null) senderName = user.Name;
        }

        Messages.Add(new ChatMessage 
        { 
            UserId = senderId,
            UserName = senderName,
            Content = content,
            IsFromCurrentUser = false
        });
    }

    private void HandleUserStatusUpdate(uint sessionId, bool isMuted, bool isDeafened)
    {
        foreach (var channel in Channels)
        {
            var user = channel.Users.FirstOrDefault(u => u.Id == sessionId);
            if (user != null)
            {
                user.IsMuted = isMuted;
                user.IsDeafened = isDeafened;
                break;
            }
        }
    }
    
    public async ValueTask DisposeAsync()
    {
        await DisconnectInternalAsync();
        _identity?.Dispose();
        GC.SuppressFinalize(this);
    }
    private void HandleUserJoined(uint channelId, uint sessionId, string name)
    {
        var channel = GetOrCreateChannel(channelId);
        
        // Check if user already exists
        if (channel.Users.Any(u => u.Id == sessionId)) return;
        
        // Don't add self
        if (_client != null && sessionId == _client.UserId) return;
        
        channel.Users.Add(new User { Id = sessionId, Name = name });
        
        // Log event
        Messages.Add(new ChatMessage { Content = $"{name} joined {channel.Name}", System = true });
    }
    
    private void HandleUserLeft(uint channelId, uint sessionId)
    {
        var channel = Channels.FirstOrDefault(c => c.Id == channelId.ToString());
        if (channel == null) return;
        
        var user = channel.Users.FirstOrDefault(u => u.Id == sessionId);
        if (user != null)
        {
            channel.Users.Remove(user);
            Messages.Add(new ChatMessage { Content = $"{user.Name} left {channel.Name}", System = true });
        }
    }
    
    private void HandleServerSnapshot(ServerState snapshot)
    {
        Console.WriteLine($"[ViewModel] Handling ServerSnapshot: {snapshot.Channels.Count} channels");
        
        // 1. Build a map of user profiles for easy lookup
        var profileMap = snapshot.Profiles.ToDictionary(p => p.UserId, p => p);
        
        // 2. Sync channels
        // We want to preserve the selected channel if possible
        var previousSelectedId = SelectedChannel?.Id;
        
        Channels.Clear();
        foreach (var chanInfo in snapshot.Channels.OrderBy(c => c.Position))
        {
            var channel = new Channel 
            { 
                Id = chanInfo.ChannelId.ToString(), 
                Name = chanInfo.Name,
                IsExpanded = true
            };
            
            foreach (var userStatus in chanInfo.Users)
            {
                uint userId = userStatus.SessionId;
                if (_client != null && userId == _client.UserId) continue; // Skip self
                
                string name = $"User {userId}";
                string comment = "";
                
                if (profileMap.TryGetValue(userId.ToString(), out var profile))
                {
                    name = profile.DisplayName;
                    comment = profile.Bio;
                }
                
                channel.Users.Add(new User 
                { 
                    Id = userId, 
                    Name = name,
                    Comment = comment,
                    IsMuted = userStatus.IsMuted,
                    IsDeafened = userStatus.IsDeafened
                });
            }
            Channels.Add(channel);
        }
        
        // 3. Restore selection or pick first
        if (previousSelectedId != null)
        {
            SelectedChannel = Channels.FirstOrDefault(c => c.Id == previousSelectedId);
        }
        
        if (SelectedChannel == null && Channels.Count > 0)
        {
            SelectedChannel = Channels[0];
        }
    }
    
    private Channel GetOrCreateChannel(uint channelId)
    {
        var idStr = channelId.ToString();
        var channel = Channels.FirstOrDefault(c => c.Id == idStr);
        if (channel == null)
        {
            channel = new Channel 
            { 
                Id = idStr, 
                Name = $"Channel {channelId}",
                IsExpanded = true
            };
            Channels.Add(channel);
        }
        return channel;
    }
    
    private void UpdateSpeakingIndicators(HashSet<uint> speakers)
    {
        foreach (var channel in Channels)
        {
            foreach (var user in channel.Users)
            {
                user.IsSpeaking = speakers.Contains(user.Id);
            }
        }
    }
    
    [RelayCommand]
    private void ToggleAudioSettings()
    {
        ShowAudioSettings = !ShowAudioSettings;
    }
}

// ==========================================================================
// Models
// ==========================================================================

public partial class Channel : ObservableObject
{
    [ObservableProperty] private string _id = "";
    [ObservableProperty] private string _name = "";
    [ObservableProperty] private bool _isExpanded;
    [ObservableProperty] private ObservableCollection<User> _users = new();
}

public partial class User : ObservableObject
{
    [ObservableProperty] private uint _id;
    [ObservableProperty] private string _name = "";
    [ObservableProperty] private string _comment = "";
    [ObservableProperty] private bool _isMuted;
    [ObservableProperty] private bool _isDeafened;
    [ObservableProperty] private bool _isSpeaking;
    [ObservableProperty] private Position3D _position = new(0, 0, 0);
}

public record Position3D(float X, float Y, float Z);

public class ChatMessage
{
    public uint UserId { get; init; }
    public string UserName { get; init; } = "";
    public string Content { get; init; } = "";
    public DateTime Timestamp { get; init; } = DateTime.Now;
    public bool IsFromCurrentUser { get; init; }
    public bool System { get; init; }
}
